# Changelog

## 0.3.0 (unreleased)

- Add per-user JSON settings for GPU selection, VRAM thresholds, growth timing,
  step size, retry cooldown, and themes. CLI options override saved settings.
- Size automatic thresholds from the selected NVIDIA card's measured VRAM.
  Select a card by index or UUID and use it for both Strata and monitoring.
- Add capacity growth mode for busy sessions. The old idle rule can wait
  indefinitely during back-to-back requests; capacity mode counts sustained
  spare VRAM and asks Strata to resize between requests.
- Retry no-op and known engine-busy responses after a cooldown. Keep uncertain
  resize failures paused and show why growth is waiting in the dashboard.
- Add read-only `--diagnose`, config initialization and validation commands,
  example card profiles, and an agent setup guide.
- Move development and release instructions out of the README.

## 0.2.1

- Add Ayu Dark, Rosé Pine, and Everforest themes.
- Fix crate verification in the release workflow.

## 0.2.0

- Publish the Rust dashboard and elastic VRAM supervisor on crates.io.
- Add selectable themes and previews that can run alongside a live session.
- Display live prefill progress and account for driver-reserved VRAM.
- Add release binaries and a Trusted Publishing workflow.
