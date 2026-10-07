//! A fixed number of threads that work through lanes of items, each lane in order.

/// Runs `lanes`, indexes into `items`, on `workers` threads: each lane on one thread in order. An
/// item in no lane keeps an empty slot.
pub(super) fn on_workers<T: Sync, R: Send>(
    items: &[T],
    lanes: &[Vec<usize>],
    workers: usize,
    run: &(dyn Fn(&T) -> R + Sync),
) -> Vec<Option<R>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::default()).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers.min(lanes.len()) {
            scope.spawn(|| {
                while let Some(lane) =
                    lanes.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    for (item, slot) in lane
                        .iter()
                        .filter_map(|at| Some((items.get(*at)?, slots.get(*at)?)))
                    {
                        let done = run(item);
                        *slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(done);
                    }
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
        .collect()
}
