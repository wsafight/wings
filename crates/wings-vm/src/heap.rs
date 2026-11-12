use std::alloc::{Layout, alloc};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Reservation, ResourceAccount, ResourceLevel, Resources};

const MAX_FIELDS: usize = 8;
const MAX_RECORDS: usize = 1024;
const MAX_LAYOUTS: usize = 128;
const MAX_ROOTS: usize = 64;
static NEXT_HEAP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Int,
    Bool,
    Unit,
    Ref,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapRef {
    heap: u64,
    slot: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordLayout {
    heap: u64,
    index: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordValue {
    Int(i64),
    Bool(bool),
    Unit,
    Ref(HeapRef),
}

#[derive(Clone, Copy)]
struct Schema {
    fields: [FieldType; MAX_FIELDS],
    count: usize,
}

#[derive(Clone, Copy)]
struct Slot {
    fields: [u64; MAX_FIELDS],
    generation: u32,
    layout: u16,
    occupied: bool,
    marked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    pub capacity: usize,
    pub allocated: usize,
    pub retired: usize,
    pub layouts: usize,
    pub collections: u64,
    pub charged_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectionStats {
    pub before: usize,
    pub retained: usize,
    pub reclaimed: usize,
}

pub struct RecordHeap {
    identity: u64,
    slots: Box<[Slot]>,
    schemas: Box<[Schema]>,
    worklist: Box<[usize]>,
    layout_count: usize,
    collections: u64,
    account: ResourceAccount,
    charge: Reservation,
}

pub struct ShadowRoots {
    heap: u64,
    slots: Box<[Option<HeapRef>]>,
    count: usize,
    charge: Reservation,
}

pub struct RootFrame<'roots> {
    roots: &'roots mut ShadowRoots,
    base: usize,
}

impl RecordHeap {
    pub fn new(
        account: &ResourceAccount,
        capacity: usize,
        layout_capacity: usize,
    ) -> Result<Self, String> {
        if account.level() != ResourceLevel::Process {
            return Err("record heap requires a process resource account".into());
        }
        if !(1..=MAX_RECORDS).contains(&capacity) || !(1..=MAX_LAYOUTS).contains(&layout_capacity) {
            return Err("record heap requires 1..=1024 records and 1..=128 layouts".into());
        }
        let bytes = std::mem::size_of::<Self>()
            + capacity * (std::mem::size_of::<Slot>() + std::mem::size_of::<usize>())
            + layout_capacity * std::mem::size_of::<Schema>();
        let charge = account.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        let identity = next_identity()?;
        let slots = allocate_array(
            capacity,
            Slot {
                fields: [0; MAX_FIELDS],
                generation: 1,
                layout: 0,
                occupied: false,
                marked: false,
            },
        )?;
        let schemas = allocate_array(
            layout_capacity,
            Schema {
                fields: [FieldType::Unit; MAX_FIELDS],
                count: 0,
            },
        )?;
        let worklist = allocate_array(capacity, 0)?;
        Ok(Self {
            identity,
            slots,
            schemas,
            worklist,
            layout_count: 0,
            collections: 0,
            account: account.clone(),
            charge,
        })
    }

    pub fn register_layout(&mut self, fields: &[FieldType]) -> Result<RecordLayout, String> {
        if fields.len() > MAX_FIELDS {
            return Err("record layout exceeds eight fields".into());
        }
        if let Some(index) = self.schemas[..self.layout_count]
            .iter()
            .position(|schema| schema.fields[..schema.count] == *fields)
        {
            return Ok(RecordLayout {
                heap: self.identity,
                index: index as u16,
            });
        }
        if self.layout_count == self.schemas.len() {
            return Err("record layout capacity exhausted".into());
        }
        let index = self.layout_count;
        self.schemas[index].fields[..fields.len()].copy_from_slice(fields);
        self.schemas[index].count = fields.len();
        self.layout_count += 1;
        Ok(RecordLayout {
            heap: self.identity,
            index: index as u16,
        })
    }

    pub fn allocate(
        &mut self,
        layout: RecordLayout,
        values: &[RecordValue],
    ) -> Result<HeapRef, String> {
        let schema = self.schema(layout)?;
        if values.len() != schema.count {
            return Err("record field count mismatch".into());
        }
        let mut fields = [0; MAX_FIELDS];
        for (index, (ty, value)) in schema.fields[..schema.count].iter().zip(values).enumerate() {
            fields[index] = match (ty, value) {
                (FieldType::Int, RecordValue::Int(value)) => *value as u64,
                (FieldType::Bool, RecordValue::Bool(value)) => u64::from(*value),
                (FieldType::Unit, RecordValue::Unit) => 0,
                (FieldType::Ref, RecordValue::Ref(reference)) => {
                    self.validate(*reference)?;
                    (u64::from(reference.generation) << 32) | u64::from(reference.slot)
                }
                _ => return Err("record field type mismatch".into()),
            };
        }
        let index = self
            .slots
            .iter()
            .position(|slot| !slot.occupied && slot.generation != 0)
            .ok_or("record heap capacity exhausted")?;
        let slot = &mut self.slots[index];
        slot.fields = fields;
        slot.layout = layout.index;
        slot.occupied = true;
        Ok(HeapRef {
            heap: self.identity,
            slot: index as u32,
            generation: slot.generation,
        })
    }

    pub fn get(&self, reference: HeapRef, field: usize) -> Result<RecordValue, String> {
        let slot = self.validate(reference)?;
        let schema = &self.schemas[usize::from(slot.layout)];
        if field >= schema.count {
            return Err("record field index out of bounds".into());
        }
        let value = slot.fields[field];
        Ok(match schema.fields[field] {
            FieldType::Int => RecordValue::Int(value as i64),
            FieldType::Bool => RecordValue::Bool(value != 0),
            FieldType::Unit => RecordValue::Unit,
            FieldType::Ref => RecordValue::Ref(self.decode_reference(value)),
        })
    }

    pub fn stats(&self) -> HeapStats {
        HeapStats {
            capacity: self.slots.len(),
            allocated: self.slots.iter().filter(|slot| slot.occupied).count(),
            retired: self
                .slots
                .iter()
                .filter(|slot| slot.generation == 0)
                .count(),
            layouts: self.layout_count,
            collections: self.collections,
            charged_bytes: self.charge.amount().bytes,
        }
    }

    pub fn collect(&mut self, roots: &ShadowRoots) -> Result<CollectionStats, String> {
        if roots.heap != self.identity {
            return Err("shadow roots belong to a different heap".into());
        }
        for reference in roots.slots[..roots.count].iter().flatten() {
            self.validate(*reference)?;
        }
        let before = self.stats().allocated;
        let mut pending = 0;
        for reference in roots.slots[..roots.count].iter().flatten() {
            self.mark(reference.slot as usize, &mut pending);
        }
        while pending != 0 {
            pending -= 1;
            let slot = self.slots[self.worklist[pending]];
            let schema = self.schemas[usize::from(slot.layout)];
            for (ty, value) in schema.fields[..schema.count].iter().zip(slot.fields) {
                if *ty == FieldType::Ref {
                    let reference = self.decode_reference(value);
                    self.mark(reference.slot as usize, &mut pending);
                }
            }
        }
        let mut retained = 0;
        for slot in &mut self.slots {
            if slot.occupied && slot.marked {
                retained += 1;
                slot.marked = false;
            } else if slot.occupied {
                slot.occupied = false;
                slot.generation = slot.generation.checked_add(1).unwrap_or(0);
            }
        }
        self.collections = self.collections.saturating_add(1);
        Ok(CollectionStats {
            before,
            retained,
            reclaimed: before - retained,
        })
    }

    fn mark(&mut self, index: usize, pending: &mut usize) {
        if !self.slots[index].marked {
            self.slots[index].marked = true;
            self.worklist[*pending] = index;
            *pending += 1;
        }
    }

    fn schema(&self, layout: RecordLayout) -> Result<&Schema, String> {
        if layout.heap != self.identity || usize::from(layout.index) >= self.layout_count {
            return Err("record layout belongs to a different heap".into());
        }
        Ok(&self.schemas[usize::from(layout.index)])
    }

    fn validate(&self, reference: HeapRef) -> Result<&Slot, String> {
        if reference.heap != self.identity {
            return Err("record reference belongs to a different heap".into());
        }
        self.slots
            .get(reference.slot as usize)
            .filter(|slot| slot.occupied && slot.generation == reference.generation)
            .ok_or_else(|| "stale record reference".into())
    }

    fn decode_reference(&self, encoded: u64) -> HeapRef {
        HeapRef {
            heap: self.identity,
            slot: encoded as u32,
            generation: (encoded >> 32) as u32,
        }
    }
}

impl ShadowRoots {
    pub fn new(heap: &RecordHeap, capacity: usize) -> Result<Self, String> {
        if !(1..=MAX_ROOTS).contains(&capacity) {
            return Err("shadow roots require 1..=64 slots".into());
        }
        let bytes = std::mem::size_of::<Self>() + capacity * std::mem::size_of::<Option<HeapRef>>();
        let charge = heap.account.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        Ok(Self {
            heap: heap.identity,
            slots: allocate_array(capacity, None)?,
            count: 0,
            charge,
        })
    }

    pub fn frame(&mut self) -> RootFrame<'_> {
        let base = self.count;
        RootFrame { roots: self, base }
    }

    pub fn charged_bytes(&self) -> u64 {
        self.charge.amount().bytes
    }
}

impl RootFrame<'_> {
    pub fn push(&mut self, heap: &RecordHeap, reference: HeapRef) -> Result<usize, String> {
        self.check_reference(heap, reference)?;
        if self.roots.count == self.roots.slots.len() {
            return Err("shadow root capacity exhausted".into());
        }
        let index = self.roots.count;
        self.roots.slots[index] = Some(reference);
        self.roots.count += 1;
        Ok(index - self.base)
    }

    pub fn set(
        &mut self,
        heap: &RecordHeap,
        index: usize,
        reference: HeapRef,
    ) -> Result<(), String> {
        if index >= self.roots.count - self.base {
            return Err("shadow root index out of bounds".into());
        }
        self.check_reference(heap, reference)?;
        self.roots.slots[self.base + index] = Some(reference);
        Ok(())
    }

    pub fn frame(&mut self) -> RootFrame<'_> {
        self.roots.frame()
    }

    pub fn collect(&self, heap: &mut RecordHeap) -> Result<CollectionStats, String> {
        heap.collect(self.roots)
    }

    fn check_reference(&self, heap: &RecordHeap, reference: HeapRef) -> Result<(), String> {
        if heap.identity != self.roots.heap {
            return Err("shadow roots belong to a different heap".into());
        }
        heap.validate(reference).map(|_| ())
    }
}

impl Drop for RootFrame<'_> {
    fn drop(&mut self) {
        self.roots.slots[self.base..self.roots.count].fill(None);
        self.roots.count = self.base;
    }
}

fn allocate_array<Value: Copy>(length: usize, value: Value) -> Result<Box<[Value]>, String> {
    let layout = Layout::array::<Value>(length).map_err(|error| error.to_string())?;
    if layout.size() == 0 {
        return Err("record storage must have a nonzero allocation size".into());
    }
    let pointer = unsafe { alloc(layout) }.cast::<Value>();
    if pointer.is_null() {
        return Err("record storage allocation failed".into());
    }
    for index in 0..length {
        unsafe { pointer.add(index).write(value) };
    }
    Ok(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(pointer, length)) })
}

fn next_identity() -> Result<u64, String> {
    let mut identity = NEXT_HEAP.load(Ordering::Relaxed);
    loop {
        let next = identity
            .checked_add(1)
            .ok_or("record heap identities exhausted")?;
        match NEXT_HEAP.compare_exchange_weak(identity, next, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return Ok(identity),
            Err(current) => identity = current,
        }
    }
}
