//! Required reclamation must preserve durable references and live neighbors.
use super::*;
use crate::test::mem_storage::{MemStorage, MemStorageState};
use crate::{DenyImplicitOpenGate, Mapping};
use std::sync::atomic::Ordering;

#[maybe_async]
async fn image(state: &Arc<MemStorageState>, reclaim: bool) -> FormatAccess<MemStorage> {
    let q = Qcow2::<MemStorage>::create_builder(MemStorage::new(state.clone(), false))
        .cluster_size(4096)
        .size(1024 * 1024)
        .create_open(DenyImplicitOpenGate::default(), |s| {
            Ok(Qcow2::<MemStorage>::builder(s).write(true).reclaim(reclaim))
        })
        .await
        .unwrap();
    FormatAccess::new(q)
}

#[maybe_async]
async fn host(image: &FormatAccess<MemStorage>, guest: u64) -> u64 {
    match image.get_mapping(guest, 1).await.unwrap().0 {
        Mapping::Raw { offset, .. } => offset,
        other => panic!("Expected allocated data, got {other:?}"),
    }
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
async fn reclaim_is_opt_in_and_preserves_neighbors() {
    for enabled in [false, true] {
        let state = Arc::new(MemStorageState::default());
        let mut q = image(&state, enabled).await;
        q.write(&vec![0x5a; 8192][..], 0).await.unwrap();
        q.flush().await.unwrap();
        q.sync().await.unwrap();
        let old = host(&q, 0).await;
        q.discard_to_zero(0, 4096).await.unwrap();
        assert_eq!(state.reclaimed.lock().len(), usize::from(enabled));
        if enabled {
            let (offset, length, durable) = state.reclaimed.lock()[0].clone();
            assert_eq!((offset, length), (old, 4096));
            // Reopen the persisted state *before* the punch: the old bytes must
            // already be unreachable, while the neighbor must still be live.
            let prior = Arc::new(MemStorageState::default());
            *prior.data.lock() = durable;
            let reopened = Qcow2::<MemStorage>::builder(MemStorage::new(prior, false))
                .open(DenyImplicitOpenGate::default())
                .await
                .unwrap();
            let reopened = FormatAccess::new(reopened);
            let mut bytes = [0xff; 8192];
            reopened.read(&mut bytes[..], 0).await.unwrap();
            assert!(bytes[..4096].iter().all(|b| *b == 0));
            assert!(bytes[4096..].iter().all(|b| *b == 0x5a));
        }
        q.write(&vec![0xa5; 4096][..], 8192).await.unwrap();
        let reused = host(&q, 8192).await;
        assert_eq!(reused, old);
        let mut neighbor = [0; 4096];
        q.read(&mut neighbor[..], 4096).await.unwrap();
        assert!(neighbor.iter().all(|b| *b == 0x5a));
        q.flush().await.unwrap();
    }
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
async fn failed_barrier_or_punch_never_reuses_the_allocation() {
    for fail_sync in [true, false] {
        let state = Arc::new(MemStorageState::default());
        let mut q = image(&state, true).await;
        q.write(&vec![0x5a; 4096][..], 0).await.unwrap();
        q.flush().await.unwrap();
        q.sync().await.unwrap();
        let old = host(&q, 0).await;
        if fail_sync {
            state.fail_next_sync.store(true, Ordering::Relaxed);
        } else {
            state.fail_next_reclaim.store(true, Ordering::Relaxed);
        }
        let result = q.discard_to_zero(0, 4096).await;
        assert!(result.is_err());
        assert!(state.reclaimed.lock().is_empty());
        q.write(&vec![0xa5; 4096][..], 4096).await.unwrap();
        let allocated = host(&q, 4096).await;
        assert_ne!(allocated, old);
        q.flush().await.unwrap();
    }
}
