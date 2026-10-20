// Prevents additional console window on Windows in release, DO NOT REMOVE!!
// The headless server build keeps its console.
#![cfg_attr(all(not(debug_assertions), feature = "desktop"), windows_subsystem = "windows")]

fn main() {
  app_lib::run();
}
