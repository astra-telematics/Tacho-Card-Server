fn main() {
  // Tauri's build step (app config, icons, capabilities) is only for the desktop app.
  #[cfg(feature = "desktop")]
  tauri_build::build()
}
