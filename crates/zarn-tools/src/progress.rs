use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use zarn_core::tools::Ctx;

pub type Sink = Arc<dyn Fn(&str, &str) + Send + Sync>;

fn sinks() -> &'static Mutex<HashMap<usize, Sink>> {
    static SINKS: OnceLock<Mutex<HashMap<usize, Sink>>> = OnceLock::new();
    SINKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn owner(ctx: &Ctx) -> usize {
    ctx as *const Ctx as usize
}

pub fn register(ctx: &Ctx, sink: Sink) {
    if let Ok(mut map) = sinks().lock() {
        map.insert(owner(ctx), sink);
    }
}

pub fn unregister(ctx: &Ctx) {
    if let Ok(mut map) = sinks().lock() {
        map.remove(&owner(ctx));
    }
}

pub fn emit(owner: usize, command: &str, line: &str) {
    let sink = sinks().lock().ok().and_then(|map| map.get(&owner).cloned());
    if let Some(sink) = sink {
        sink(command, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_reach_only_the_sink_of_their_context() {
        let a = Ctx::new(".");
        let b = Ctx::new(".");
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = seen.clone();
        register(&a, Arc::new(move |cmd, line| log.lock().unwrap().push(format!("{cmd}: {line}"))));
        emit(owner(&a), "ls", "x");
        emit(owner(&b), "ls", "y");
        unregister(&a);
        emit(owner(&a), "ls", "z");
        assert_eq!(*seen.lock().unwrap(), vec!["ls: x".to_string()]);
    }
}
