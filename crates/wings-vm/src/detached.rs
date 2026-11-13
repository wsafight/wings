use crate::{ScalarModule, ScalarProcess, StackPool, aot_heap::AotHeap};

pub struct DetachedProcess {
    image: Option<ProcessImage>,
}

struct ProcessImage {
    module: ScalarModule,
    state: i64,
    failed: bool,
    heap: Option<Box<AotHeap>>,
}

impl ScalarProcess {
    pub fn detach(self) -> DetachedProcess {
        let Self {
            module,
            state,
            failed,
            heap,
            pool,
        } = self;
        drop(pool);
        DetachedProcess {
            image: Some(ProcessImage {
                module,
                state,
                failed,
                heap,
            }),
        }
    }
}

impl DetachedProcess {
    pub fn attach(&mut self, pool: &StackPool) -> Result<ScalarProcess, String> {
        let image = self
            .image
            .as_ref()
            .ok_or("process image has already been attached")?;
        image.module.validate_pool(pool)?;
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
            state: image.state,
            failed: image.failed,
            heap: image.heap,
            pool: Some(pool.clone()),
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
