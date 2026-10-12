//! Нативная электромагнитная лаборатория; вычисления выполняются отдельным worker.
mod cli;

use std::{io::Write, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Если stderr закрыт, всё равно возвращаем неуспешный код без panic.
            let _ = writeln!(
                std::io::stderr().lock(),
                "Volna: {error}\nСправка: volna --help"
            );
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let startup = match cli::parse(std::env::args_os().skip(1))? {
        cli::Action::Help => {
            return std::io::stdout()
                .lock()
                .write_all(cli::USAGE.as_bytes())
                .map_err(|error| format!("Не удалось вывести справку: {error}"));
        }
        cli::Action::Run(options) => options.load()?,
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([1000.0, 700.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Volna — электромагнитная лаборатория",
        options,
        Box::new(move |cc| {
            Ok(Box::new(volna::ui::VolnaApp::new_with_options(
                cc, startup,
            )?))
        }),
    )
    .map_err(|error| format!("Не удалось запустить GUI: {error}"))
}
