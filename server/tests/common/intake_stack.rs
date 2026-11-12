use std::future::Future;

// These intake tests poll the HTTP route and embedded SurrealDB on one thread.
// The coverage-instrumented call stack exceeds libtest's default stack.
pub fn run<F: Future<Output = ()> + 'static>(scenario: fn() -> F) {
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("intake test runtime")
                .block_on(scenario());
        })
        .expect("intake test thread")
        .join();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
