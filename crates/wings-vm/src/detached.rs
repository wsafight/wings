use crate::{ResourceAccount, ScalarModule, ScalarProcess, StackPool, aot_heap::AotHeap};

pub struct DetachedProcess {
    image: Option<ProcessImage>,
}

struct ProcessImage {
    module: ScalarModule,
    account: Option<ResourceAccount>,
    state: i64,
    failed: bool,
    heap: Option<Box<AotHeap>>,
}

impl ScalarProcess {
    pub fn detach(self) -> DetachedProcess {
        let Self {
            module,
            account,
            state,
            failed,
            heap,
            pool,
        } = self;
        drop(pool);
        DetachedProcess {
            image: Some(ProcessImage {
                module,
                account,
                state,
                failed,
                heap,
            }),
        }
    }
}

impl DetachedProcess {
    pub(crate) fn byte_state_snapshot(&self) -> Option<crate::ByteStateSnapshot> {
        let image = self.image.as_ref()?;
        if image.module.manifest().state != wings_abi::ValueType::BytesRef {
            return None;
        }
        image.heap.as_deref()?.byte_state_snapshot()
    }

    pub fn attach(&mut self, pool: &StackPool) -> Result<ScalarProcess, String> {
        let image = self
            .image
            .as_ref()
            .ok_or("process image has already been attached")?;
        image.module.validate_pool(pool)?;
        if image
            .account
            .as_ref()
            .is_some_and(|account| !pool.is_for_process(account))
        {
            return Err(
                "process image requires its original process-accounted stack pool binding".into(),
            );
        }
        if image
            .heap
            .as_ref()
            .is_some_and(|heap| !pool.is_for_process(&heap.account))
        {
            return Err(
                "process image requires its original process-accounted stack pool binding".into(),
            );
        }
        let image = self.image.take().expect("validated process image");
        Ok(ScalarProcess {
            module: image.module,
            account: image.account,
            state: image.state,
            failed: image.failed,
            heap: image.heap,
            pool: Some(pool.clone()),
        })
    }

    pub(crate) fn attach_scheduled(
        &mut self,
        pool: &StackPool,
        process_account: &ResourceAccount,
    ) -> Result<ScalarProcess, String> {
        let image = self
            .image
            .as_ref()
            .ok_or("process image has already been attached")?;
        if let Some(tenant) = pool.account() {
            image.module.validate_vm(tenant)?;
        }
        if image.module.inner.lease.is_some()
            && process_account.level() != crate::ResourceLevel::Process
        {
            return Err("accounted module requires a process-accounted stack pool".into());
        }
        if image
            .account
            .as_ref()
            .is_some_and(|account| !account.same(process_account))
        {
            return Err(
                "process image requires its original process-accounted stack pool binding".into(),
            );
        }
        if image
            .heap
            .as_ref()
            .is_some_and(|heap| !heap.account.same(process_account))
        {
            return Err(
                "process image requires its original process-accounted stack pool binding".into(),
            );
        }
        let image = self.image.take().expect("validated process image");
        Ok(ScalarProcess {
            module: image.module,
            account: image.account,
            state: image.state,
            failed: image.failed,
            heap: image.heap,
            pool: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_images_and_managed_heap_storage_are_send_without_unsafe_impls() {
        fn assert_send<Value: Send>() {}
        assert_send::<DetachedProcess>();
        assert_send::<AotHeap>();
    }
}
