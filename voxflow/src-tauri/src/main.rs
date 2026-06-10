// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Диагностический прогон пайплайна без GUI: voxflow.exe --selftest <16k.wav>
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--selftest" {
        voxflow_lib::selftest(&args[2]);
        return;
    }
    // Headless-проверка живого стриминга: voxflow.exe --stream-selftest <16k.wav>
    if args.len() >= 3 && args[1] == "--stream-selftest" {
        voxflow_lib::stream_selftest(&args[2]);
        return;
    }
    // Headless-проверка облачного STT: voxflow.exe --stt-test <wav>
    if args.len() >= 3 && args[1] == "--stt-test" {
        voxflow_lib::stt_test_cli(&args[2]);
        return;
    }
    // Headless-проверка GigaAM (русский ASR): voxflow.exe --gigaam-selftest <wav>
    if args.len() >= 3 && args[1] == "--gigaam-selftest" {
        voxflow_lib::gigaam_selftest(&args[2]);
        return;
    }
    voxflow_lib::run()
}
