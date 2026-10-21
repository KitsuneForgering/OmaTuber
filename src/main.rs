use eframe::egui;
use omatuber::app::{App, Config};
use omatuber::{cli, server};

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = cli::run(&args, server::DEFAULT_PORT) {
        std::process::exit(code);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OmaTuber")
            .with_app_id("omatuber")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([420.0, 520.0]),
        ..Default::default()
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            std::process::exit(1);
        }
    };
    eframe::run_native(
        "OmaTuber",
        options,
        Box::new(|cc| {
            Ok(Box::new(App::new(
                runtime,
                &cc.egui_ctx,
                Config::from_env(),
            )))
        }),
    )
}
