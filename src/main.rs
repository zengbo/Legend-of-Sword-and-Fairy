//! rustpal - a Rust reimplementation of the PAL (Legend of Sword and Fairy)
//! DOS engine.

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut offscreen = false;
        let mut mute = false;
        let mut console = false;
        let mut console_mode = "auto";
        #[cfg(feature = "gui")]
        let mut ui_driver = None::<String>;

        for argument in std::env::args().skip(1) {
            match argument.as_str() {
                #[cfg(feature = "gui")]
                "--ui-driver" => {
                    ui_driver = Some(rustpal::ui_driver::DEFAULT_BIND.to_owned());
                }
                "--offscreen" => offscreen = true,
                "--mute" => mute = true,
                "--console" => {
                    console = true;
                    console_mode = "auto";
                }
                "--console=kitty" => {
                    console = true;
                    console_mode = "kitty";
                }
                "--console=ansi" => {
                    console = true;
                    console_mode = "ansi";
                }
                "--help" | "-h" => {
                    print_help();
                    return;
                }
                other => {
                    if let Some(n) = other.strip_prefix("--console-scale=") {
                        std::env::set_var("RUSTPAL_CONSOLE_SCALE", n);
                        continue;
                    }
                    #[cfg(feature = "gui")]
                    if let Some(bind) = other.strip_prefix("--ui-driver=") {
                        ui_driver = Some(bind.to_owned());
                        continue;
                    }
                    eprintln!("rustpal: unknown argument {other:?}; try --help");
                    std::process::exit(2);
                }
            }
        }

        #[cfg(feature = "gui")]
        if let Some(bind) = ui_driver {
            std::env::set_var("RUSTPAL_UI_DRIVER", bind);
        }
        if offscreen {
            std::env::set_var("RUSTPAL_OFFSCREEN", "1");
        }
        if console {
            std::env::set_var("RUSTPAL_CONSOLE", "1");
            std::env::set_var("RUSTPAL_CONSOLE_MODE", console_mode);
        }
        if offscreen || mute || console {
            std::env::set_var("RUSTPAL_DISABLE_AUDIO", "1");
        }

        #[cfg(feature = "console")]
        if console {
            match rustpal::game_loop::Engine::with_backend(
                rustpal::game_loop::VideoBackend::Console,
            ) {
                Ok(mut engine) => engine.run(),
                Err(e) => {
                    eprintln!("rustpal: failed to start: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }

        #[cfg(not(feature = "console"))]
        if console {
            eprintln!("rustpal: --console requires the `console` feature");
            std::process::exit(2);
        }
    }

    match rustpal::game_loop::Engine::new(false) {
        Ok(mut engine) => engine.run(),
        Err(e) => {
            eprintln!("rustpal: failed to start: {e}");
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!(
        "Usage: rustpal [options]\n\n\
         --console           Terminal video (Kitty pixels or ANSI half-blocks)\n\
         --console=kitty     Force Kitty graphics protocol\n\
         --console=ansi      Force ANSI half-block rendering\n\
         --console-scale=N   Integer upscale (1–16); default auto-fits the terminal\n\
         --offscreen         GUI: render without a visible window\n\
         --mute              Disable music and sound effects"
    );
    #[cfg(feature = "gui")]
    println!(
        "--ui-driver         Enable local control API at {}\n\
         --ui-driver=ADDR    Enable it at a loopback IP and port",
        rustpal::ui_driver::DEFAULT_BIND
    );
    println!(
        "\nConsole-only (no system GUI/audio libs):\n\
         cargo build --release --no-default-features --features console\n\
         ./target/release/rustpal --console"
    );
}
