#[doc(hidden)]
#[macro_export]
macro_rules! frb_generated_moi_arc_def {
    () => {
        use std::collections::HashMap;
        use std::marker::PhantomData;
        use std::sync::Arc;

        // EasyNotes patch (see third_party/README.md): the pool's lock is taken by
        // asking until it is free, on the web.
        //
        // A browser also takes this lock on the page's main thread: every call
        // Dart makes on an object clones its handle there, a `#[frb(sync)]` call
        // drops one there, and so does the finalizer. A contended `RwLock` waits
        // with `Atomics.wait`, which throws on a main thread, so without this a
        // handle released on a worker at the wrong moment throws
        // `Atomics.wait cannot be called in this context` out of a synchronous
        // call. Nothing that holds the lock waits for anything while it does,
        // so the spin is as short as a map operation.
        #[cfg(target_family = "wasm")]
        fn moi_arc_pool_read<T: ?Sized>(
            pool: &MoiArcPool<T>,
        ) -> std::sync::LockResult<std::sync::RwLockReadGuard<'_, MoiArcPoolInner<T>>> {
            loop {
                match pool.try_read() {
                    Ok(guard) => return Ok(guard),
                    Err(std::sync::TryLockError::Poisoned(poisoned)) => return Err(poisoned),
                    Err(std::sync::TryLockError::WouldBlock) => std::hint::spin_loop(),
                }
            }
        }

        #[cfg(target_family = "wasm")]
        fn moi_arc_pool_write<T: ?Sized>(
            pool: &MoiArcPool<T>,
        ) -> std::sync::LockResult<std::sync::RwLockWriteGuard<'_, MoiArcPoolInner<T>>> {
            loop {
                match pool.try_write() {
                    Ok(guard) => return Ok(guard),
                    Err(std::sync::TryLockError::Poisoned(poisoned)) => return Err(poisoned),
                    Err(std::sync::TryLockError::WouldBlock) => std::hint::spin_loop(),
                }
            }
        }

        #[cfg(not(target_family = "wasm"))]
        fn moi_arc_pool_read<T: ?Sized>(
            pool: &MoiArcPool<T>,
        ) -> std::sync::LockResult<std::sync::RwLockReadGuard<'_, MoiArcPoolInner<T>>> {
            pool.read()
        }

        #[cfg(not(target_family = "wasm"))]
        fn moi_arc_pool_write<T: ?Sized>(
            pool: &MoiArcPool<T>,
        ) -> std::sync::LockResult<std::sync::RwLockWriteGuard<'_, MoiArcPoolInner<T>>> {
            pool.write()
        }

        #[derive(Debug)]
        pub struct MoiArc<T: ?Sized + MoiArcValue> {
            // `Option` for correct dropping
            object_id: Option<MoiObjectId>,
            // Mainly for `as_ref`. `Option` for correct dropping
            value: Option<Arc<T>>,
            _phantom: PhantomData<T>,
        }

        impl<T: ?Sized + MoiArcValue> Drop for MoiArc<T> {
            fn drop(&mut self) {
                if let Some(object_id) = self.object_id {
                    Self::decrement_strong_count(object_id);
                }
            }
        }

        impl<T: ?Sized + MoiArcValue> AsRef<T> for MoiArc<T> {
            fn as_ref(&self) -> &T {
                self.value.as_ref().unwrap().as_ref()
            }
        }

        impl<T: ?Sized + MoiArcValue> $crate::for_generated::BaseArc<T> for MoiArc<T> {
            fn new(value: T) -> Self
            where
                T: Sized,
            {
                let mut pool = moi_arc_pool_write(T::get_pool()).unwrap();
                let object_id = pool.id_generator.next_id();

                let value = Arc::new(value);

                let old_value = pool.map.insert(
                    object_id,
                    MoiArcPoolValue {
                        ref_count: 1,
                        value: value.clone(),
                    },
                );
                assert!(old_value.is_none());

                Self {
                    object_id: Some(object_id),
                    value: Some(value),
                    _phantom: PhantomData,
                }
            }

            fn try_unwrap(mut self) -> Result<T, Self>
            where
                T: Sized,
            {
                // NOTE: Ensure lock is held during all operations to avoid racing conditions
                let pool = &mut moi_arc_pool_write(T::get_pool()).unwrap();

                if pool.map.get(&self.object_id.unwrap()).unwrap().ref_count == 1 {
                    Self::decrement_strong_count_raw(self.object_id.unwrap(), pool);
                    // `take`, such that the `drop` will not decrease ref count
                    self.object_id.take().unwrap();
                    Ok(Arc::into_inner(self.value.take().unwrap()).unwrap())
                } else {
                    Err(self)
                }
            }

            fn into_inner(self) -> Option<T>
            where
                T: Sized,
            {
                self.try_unwrap().ok()
            }

            fn into_raw(mut self) -> usize {
                // `take`, such that the `drop` will not decrease ref count
                self.object_id.take().unwrap()
            }
        }

        impl<T: ?Sized + MoiArcValue> Clone for MoiArc<T> {
            fn clone(&self) -> Self {
                Self::increment_strong_count(self.object_id.unwrap());

                Self {
                    object_id: self.object_id,
                    value: self.value.clone(),
                    _phantom: PhantomData,
                }
            }
        }

        impl<T: ?Sized + MoiArcValue> MoiArc<T> {
            pub(crate) fn from_raw(raw: usize) -> Self
            where
                T: Sized,
            {
                let map = &moi_arc_pool_read(T::get_pool()).unwrap().map;

                Self {
                    object_id: Some(raw),
                    value: Some(map.get(&raw).unwrap().value.clone()),
                    _phantom: PhantomData,
                }
            }

            pub fn increment_strong_count(raw: usize) {
                let map = &mut moi_arc_pool_write(T::get_pool()).unwrap().map;
                map.get_mut(&raw).unwrap().ref_count += 1;
            }

            pub fn decrement_strong_count(raw: usize) {
                let mut pool = moi_arc_pool_write(T::get_pool()).unwrap();
                let object = Self::decrement_strong_count_raw(raw, &mut pool);
                drop(pool);
                drop(object);
            }

            fn decrement_strong_count_raw(
                raw: usize,
                pool: &mut MoiArcPoolInner<T>,
            ) -> Option<MoiArcPoolValue<T>> {
                let value = pool.map.get_mut(&raw).unwrap();
                value.ref_count -= 1;
                if value.ref_count == 0 {
                    pool.map.remove(&raw)
                } else {
                    None
                }
            }
        }

        pub trait MoiArcValue: 'static {
            fn get_pool() -> &'static MoiArcPool<Self>;
        }

        type MoiObjectId = usize;

        pub type MoiArcPool<T> = std::sync::RwLock<MoiArcPoolInner<T>>;

        pub struct MoiArcPoolInner<T: ?Sized> {
            map: HashMap<MoiObjectId, MoiArcPoolValue<T>>,
            id_generator: IdGenerator,
        }

        impl<T: ?Sized> Default for MoiArcPoolInner<T> {
            fn default() -> Self {
                Self {
                    map: HashMap::new(),
                    id_generator: Default::default(),
                }
            }
        }

        struct IdGenerator {
            next_id: MoiObjectId,
        }

        impl Default for IdGenerator {
            fn default() -> Self {
                Self {
                    next_id: Self::MIN_ID,
                }
            }
        }

        impl IdGenerator {
            const MIN_ID: MoiObjectId = 1;
            // Less than i32's max value to be extra safe
            const MAX_ID: MoiObjectId = 2147483600;

            fn next_id(&mut self) -> MoiObjectId {
                let ans = self.next_id;

                self.next_id = if self.next_id >= Self::MAX_ID {
                    Self::MIN_ID
                } else {
                    self.next_id + 1
                };

                ans
            }
        }

        impl<T: ?Sized> MoiArcPoolInner<T> {}

        struct MoiArcPoolValue<T: ?Sized> {
            // Real reference counting of this MoiArc
            ref_count: i32,
            // Use (std) Arc merely for lifetime
            value: Arc<T>,
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! frb_generated_moi_arc_impl_value {
    ($T:ty) => {
        impl MoiArcValue for $T {
            fn get_pool() -> &'static MoiArcPool<Self> {
                $crate::for_generated::lazy_static! {
                    static ref POOL: MoiArcPool<$T> = MoiArcPool::new(Default::default());
                }
                &POOL
            }
        }
    };
}

#[cfg(test)]
mod tests {
    crate::frb_generated_moi_arc_def!();

    #[test]
    fn test_next_id() {
        let mut pool = MoiArcPoolInner::<String>::default();
        assert_eq!(pool.id_generator.next_id(), 1);
        assert_eq!(pool.id_generator.next_id(), 2);
        assert_eq!(pool.id_generator.next_id(), 3);

        pool.id_generator.next_id = 2147483598; // HACK and change value
        assert_eq!(pool.id_generator.next_id(), 2147483598);
        assert_eq!(pool.id_generator.next_id(), 2147483599);
        assert_eq!(pool.id_generator.next_id(), 2147483600);
        assert_eq!(pool.id_generator.next_id(), 1); // NOTE: still not zero
        assert_eq!(pool.id_generator.next_id(), 2);
        assert_eq!(pool.id_generator.next_id(), 3);
    }

    frb_generated_moi_arc_impl_value!(DummyType);

    crate::base_arc_generate_tests!(MoiArc);
}
