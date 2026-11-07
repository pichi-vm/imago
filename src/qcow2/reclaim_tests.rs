//! Required reclamation must preserve durable references and live neighbors.

use super::*;
use crate::test::mem_storage::{MemStorage, MemStorageState};
use crate::{DenyImplicitOpenGate, Mapping};

#[maybe_async]
async fn create_image(
    state: &Arc<MemStorageState>,
    reclaim: bool,
    align: usize,
) -> FormatAccess<MemStorage> {
    let qcow2 = Qcow2::<MemStorage>::create_builder(
        MemStorage::new(Arc::clone(state), false).with_align(align),
    )
    .cluster_size(4096)
    .size(1024 * 1024)
    .create_open(DenyImplicitOpenGate::default(), |s| {
        Ok(Qcow2::<MemStorage>::builder(s).write(true).reclaim(reclaim))
    })
    .await
    .unwrap();
    FormatAccess::new(qcow2)
}

#[maybe_async]
async fn host_offset(image: &FormatAccess<MemStorage>, guest: u64) -> u64 {
    match image.get_mapping(guest, 1).await.unwrap().0 {
        Mapping::Raw { offset, .. } => offset,
        other => panic!("Expected allocated data, got {other:?}"),
    }
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
async fn test_reclaim_is_opt_in_and_preserves_neighbors() {
    for enabled in [false, true] {
        let state = Arc::new(MemStorageState::default());
        let mut image = create_image(&state, enabled, 1).await;
        image.write(&vec![0x5a; 8192][..], 0).await.unwrap();
        image.flush().await.unwrap();
        image.sync().await.unwrap();
        let old = host_offset(&image, 0).await;
        // Safe: The image was flushed and synced, and no other accesses are in flight.
        unsafe { image.invalidate_cache() }.await.unwrap();
        image.discard_to_zero(0, 4096).await.unwrap();
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
        image.write(&vec![0xa5; 4096][..], 8192).await.unwrap();
        let reused = host_offset(&image, 8192).await;
        assert_eq!(reused, old);
        let mut neighbor = [0; 4096];
        image.read(&mut neighbor[..], 4096).await.unwrap();
        assert!(neighbor.iter().all(|b| *b == 0x5a));
        image.flush().await.unwrap();
    }
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
async fn test_reclaim_respects_host_alignment() {
    for align in [1, 512, 4096] {
        let state = Arc::new(MemStorageState::default());
        let mut image = create_image(&state, true, align).await;
        image.write(&vec![0x5a; 8192][..], 0).await.unwrap();
        let old = host_offset(&image, 0).await;
        image.discard_to_zero(0, 4096).await.unwrap();
        {
            let reclaimed = state.reclaimed.lock();
            assert_eq!(reclaimed.len(), 1);
            assert_eq!((reclaimed[0].0, reclaimed[0].1), (old, 4096));
        }
        let mut bytes = [0xff; 8192];
        image.read(&mut bytes[..], 0).await.unwrap();
        assert!(bytes[..4096].iter().all(|b| *b == 0));
        assert!(bytes[4096..].iter().all(|b| *b == 0x5a));
    }
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
async fn test_unsupported_reclamation_does_not_open_dependencies() {
    let state = Arc::new(MemStorageState::default());
    let data = MemStorage::new(Arc::new(MemStorageState::default()), false);
    let image = Qcow2::<MemStorage>::create_builder(MemStorage::new(Arc::clone(&state), false))
        .size(1024 * 1024)
        .data_file("external".into(), data)
        .create_open(DenyImplicitOpenGate::default(), |s| {
            Ok(Qcow2::<MemStorage>::builder(s))
        })
        .await
        .unwrap();
    image.flush().await.unwrap();
    drop(image);
    let result = Qcow2::<MemStorage>::builder(MemStorage::new(state, false))
        .write(true)
        .reclaim(true)
        .backing_path("must-not-open", Format::Raw)
        .open(DenyImplicitOpenGate::default())
        .await;
    // The policy must reject the image before the gate rejects an implicit open.
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
async fn test_reclaim_rejects_larger_host_alignment() {
    let state = Arc::new(MemStorageState::default());
    let image = create_image(&state, false, 8192).await;
    image.flush().await.unwrap();
    drop(image);
    let result = Qcow2::<MemStorage>::builder(MemStorage::new(state, false).with_align(8192))
        .write(true)
        .reclaim(true)
        .backing_path("must-not-open", Format::Raw)
        .open(DenyImplicitOpenGate::default())
        .await;
    // Reject the incompatible policy before trying to open dependencies.
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
}
