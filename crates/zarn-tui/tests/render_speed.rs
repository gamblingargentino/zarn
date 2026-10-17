use std::time::{Duration, Instant};
use zarn_tui::state::{App, Block};

fn reply(n: usize) -> String {
    format!(
        "## Part {n}\n\nThis is **paragraph** {n} with `code`, a [link](https://example.com) and enough words to wrap across the width of a normal terminal window several times over, so the wrapper has real work to do.\n\n- first point about {n}\n- second point with *emphasis*\n\n```rust\nfn main() {{\n    println!(\"{n}\");\n}}\n```\n\n| a | b |\n| --- | --- |\n| {n} | {n} |\n"
    )
}

type Term = ratatui::Terminal<ratatui::backend::TestBackend>;

fn terminal() -> Term {
    ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap()
}

fn draw(terminal: &mut Term, app: &App) -> Duration {
    let start = Instant::now();
    terminal.draw(|f| zarn_tui::render::draw(f, app)).unwrap();
    start.elapsed()
}

fn streamed(terminal: &mut Term, app: &mut App, frames: u32) -> Duration {
    let mut total = Duration::ZERO;
    for n in 0..frames {
        if let Some(Block::Assistant(text)) = app.blocks.last_mut() {
            text.push_str(&format!(" more streamed text {n}"));
        }
        total += draw(terminal, app);
    }
    total / frames
}

#[test]
fn a_long_conversation_redraws_quickly() {
    let mut app = App::new("m");
    for n in 0..2_000 {
        app.blocks.push(Block::User(format!("question {n}")));
        app.blocks.push(Block::Assistant(reply(n)));
    }
    let mut terminal = terminal();
    let first = draw(&mut terminal, &app);
    let resized = { terminal.backend_mut().resize(100, 40); draw(&mut terminal, &app) };
    let steady = (0..20).map(|_| draw(&mut terminal, &app)).sum::<Duration>() / 20;
    let streaming = streamed(&mut terminal, &mut app, 20);
    eprintln!("first frame {first:?}, resized frame {resized:?}, steady frame {steady:?}, streaming frame {streaming:?}");
    let limit = Duration::from_millis(if cfg!(debug_assertions) { 250 } else { 25 });
    assert!(resized < limit, "resized frame took {resized:?}");
    assert!(steady < limit, "steady frame took {steady:?}");
    assert!(streaming < limit, "streaming frame took {streaming:?}");
}

#[test]
fn one_huge_streaming_reply_stays_responsive() {
    let mut app = App::new("m");
    app.blocks.push(Block::User("write a lot".into()));
    app.blocks.push(Block::Assistant((0..400).map(reply).collect()));
    let mut terminal = terminal();
    draw(&mut terminal, &app);
    let streaming = streamed(&mut terminal, &mut app, 10);
    eprintln!("huge reply streaming frame {streaming:?}");
    let limit = Duration::from_millis(if cfg!(debug_assertions) { 250 } else { 25 });
    assert!(streaming < limit, "streaming frame took {streaming:?}");
}
