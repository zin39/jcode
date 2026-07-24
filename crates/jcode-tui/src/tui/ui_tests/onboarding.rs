use super::*;
use ratatui::backend::TestBackend;
use ratatui::{Terminal, layout::Rect};

/// Render the onboarding welcome screen for the given state at the given size
/// and return the flattened text of the whole buffer.
fn render_onboarding(state: &TestState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, width, height);
            crate::tui::ui::onboarding::draw_onboarding_welcome(frame, state, area);
        })
        .expect("failed to draw onboarding");

    let buf = terminal.backend().buffer();
    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::with_capacity(width as usize);
        for x in 0..width {
            line.push_str(buf[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

fn onboarding_state() -> TestState {
    TestState {
        onboarding_preview: true,
        suggestions: vec![
            ("Log in to get started".to_string(), "/login".to_string()),
            (
                "Build a small CLI tool".to_string(),
                "build a CLI".to_string(),
            ),
        ],
        ..Default::default()
    }
}

#[test]
fn onboarding_welcome_shows_telemetry_title_and_suggestions() {
    let state = onboarding_state();
    let text = render_onboarding(&state, 80, 30);

    assert!(
        text.contains("anonymous usage statistics"),
        "telemetry notice should be rendered:\n{text}"
    );
    assert!(
        text.contains("JCODE_NO_TELEMETRY=1"),
        "telemetry opt-out hint should be rendered:\n{text}"
    );
    assert!(
        text.contains("Welcome to jcode onboarding"),
        "welcome title should be rendered:\n{text}"
    );
    assert!(
        text.contains("Log in to get started"),
        "login suggestion should be rendered:\n{text}"
    );
    assert!(
        text.contains("Build a small CLI tool"),
        "secondary suggestion should be rendered:\n{text}"
    );
    assert!(
        text.contains("Press 1-2 or type anything to start"),
        "numeric hint should reflect suggestion count:\n{text}"
    );
}

#[test]
fn onboarding_welcome_login_suggestion_shows_typed_command() {
    let state = onboarding_state();
    let text = render_onboarding(&state, 80, 30);
    assert!(
        text.contains("(type /login)"),
        "login suggestion should hint the slash command:\n{text}"
    );
}

#[test]
fn onboarding_welcome_renders_on_tiny_area_without_panicking() {
    // Below the donut/full-treatment threshold: should fall back gracefully.
    // The title may be truncated at narrow widths, so only assert its prefix.
    let state = onboarding_state();
    let text = render_onboarding(&state, 20, 5);
    assert!(
        text.contains("Welcome to jcode"),
        "minimal fallback should still show the title:\n{text}"
    );
}

#[test]
fn onboarding_welcome_centers_within_tall_area() {
    // A tall area should leave blank padding above the telemetry header.
    let state = onboarding_state();
    let text = render_onboarding(&state, 80, 40);
    let first_nonblank = text
        .lines()
        .position(|line| !line.trim().is_empty())
        .expect("expected some content");
    assert!(
        first_nonblank > 0,
        "content should be vertically padded from the top:\n{text}"
    );
}

/// Render the empty-state chat screen (initial transcript with no messages)
/// via `draw` and return the flattened text of the whole buffer.
fn render_empty_chat(state: &TestState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    terminal
        .draw(|frame| {
            crate::tui::ui::draw(frame, state);
        })
        .expect("failed to draw empty chat");

    let buf = terminal.backend().buffer();
    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::with_capacity(width as usize);
        for x in 0..width {
            line.push_str(buf[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

#[test]
fn empty_session_shows_wordmark_and_three_ghost_prompts() {
    let state = TestState {
        welcome_suggestions: vec![
            ("Explain this codebase".to_string(), "Explain the codebase".to_string()),
            ("Add error handling in a recent file".to_string(), "Add error handling".to_string()),
            ("Fix the failing tests".to_string(), "Fix failing tests".to_string()),
        ],
        ..Default::default()
    };
    let text = render_empty_chat(&state, 100, 30);

    assert!(
        text.contains("❯ jcode"),
        "wordmark should be present in empty session:\n{text}"
    );
    assert!(
        text.contains("1  Explain this codebase"),
        "first ghost prompt should be rendered:\n{text}"
    );
    assert!(
        text.contains("2  Add error handling in a recent file"),
        "second ghost prompt should be rendered:\n{text}"
    );
    assert!(
        text.contains("3  Fix the failing tests"),
        "third ghost prompt should be rendered:\n{text}"
    );
}

#[test]
fn empty_session_without_welcome_prompts_shows_wordmark_only() {
    let state = TestState::default();
    let text = render_empty_chat(&state, 100, 30);

    assert!(
        text.contains("❯ jcode"),
        "wordmark should be present even without ghost prompts (no blank screen):\n{text}"
    );
    assert!(
        !text.contains("1  Explain this codebase"),
        "no ghost prompts should be rendered when welcome_suggestions is empty:\n{text}"
    );
}

#[test]
fn non_empty_session_renders_messages_not_welcome() {
    let state = TestState {
        display_messages: vec![DisplayMessage::user("hello")],
        welcome_suggestions: vec![
            ("Explain this codebase".to_string(), "Explain the codebase".to_string()),
        ],
        ..Default::default()
    };
    let text = render_empty_chat(&state, 100, 30);

    assert!(
        text.contains("hello"),
        "user message should be rendered:\n{text}"
    );
    assert!(
        !text.contains("❯ jcode"),
        "wordmark should NOT appear when messages exist:\n{text}"
    );
    assert!(
        !text.contains("Explain this codebase"),
        "ghost prompts should NOT appear when messages exist:\n{text}"
    );
}
