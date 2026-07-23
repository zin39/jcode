//! Temporary probe: measure where an idle full-frame draw spends its time.
//! Run with: cargo test -p jcode-tui --profile selfdev frame_cost_probe -- --nocapture --ignored
use super::*;

#[test]
#[ignore]
fn frame_cost_probe_idle_screen() {
    // Mirror the slow-frame logs: 222x70 and 110x35 idle screens with 1 message.
    for (w, h) in [(222u16, 70u16), (110, 35)] {
        let state = TestState {
            display_messages: vec![DisplayMessage::system("welcome".to_string())],
            input: String::new(),
            ..Default::default()
        };
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        // Warm caches once.
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .unwrap();
        let start = std::time::Instant::now();
        const N: usize = 20;
        for _ in 0..N {
            terminal
                .draw(|frame| crate::tui::ui::draw(frame, &state))
                .unwrap();
        }
        let per_frame = start.elapsed().as_secs_f64() * 1000.0 / N as f64;
        eprintln!("PROBE size={}x{} draw_ms_avg={:.2}", w, h, per_frame);
    }
}
