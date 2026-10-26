use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};
use anyhow::anyhow;
use rexpect::error::Error;
use tracing::{debug, dispatcher, trace};
use rexpect::session::{Options, PtySession};
use crate::models::os_command::{OsCommand, SendType};
use crate::utils::log::setup_experiment_logger;
use crate::utils::qemu::vm::QemuVm;

const TARGET: &str = "telnet";

//const TIMEOUT: Duration = Duration::from_secs(240);

pub fn execute_commands_from_node(experiment_run_path: &PathBuf, node_name: &str, vm: &QemuVm, commands: Vec<OsCommand>, timeout: Option<u64>, remote_stop: Option<Arc<AtomicBool>>) -> anyhow::Result<()> {
    execute_commands(&experiment_run_path, &node_name, &vm.console_host, vm.console_port, commands, timeout, remote_stop)
}

pub fn execute_commands(
    experiment_run_path: &PathBuf,
    node_name: &str,
    console_host: &str,
    console: u16,
    commands: Vec<OsCommand>,
    timeout: Option<u64>,
    remote_stop: Option<Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    let (dispatcher, _file_guard) = setup_experiment_logger(&experiment_run_path, &node_name)?;

    dispatcher::with_default(&dispatcher, || -> anyhow::Result<()> {
        let mut command = Command::new("telnet");

        command.args(vec![console_host, console.to_string().as_str()]);

        let mut telnet = rexpect::spawn_with_options(
            command,
            Options::new()
                .timeout_ms(timeout)
                .strip_ansi_escape_codes(true)
        )?;

        debug!(target: TARGET, "Executing commands for {} (timeout: {} seconds)", node_name, timeout.unwrap_or(0) / 1_000);

        telnet.send_line("")?;


        for (index, command) in commands.iter().enumerate() {
            debug!(target: "tx", "{: >2}/{: >2} > {}", index+1, commands.len(), &command.send);

            wait_loop(&mut telnet, &command, &remote_stop)?;
            send_command(&mut telnet, &command, &remote_stop)?;
        }

        sleep(Duration::from_millis(100));

        debug!(target: TARGET, "End commands for {}", node_name);

        //let _ = telnet.send_line("");
        let _ = telnet.flush();

        telnet.process_mut().exit()?;

        Ok(())
    })
}

fn wait_loop(telnet: &mut PtySession, command: &OsCommand, remote_stop: &Option<Arc<AtomicBool>>) -> anyhow::Result<()> {
    'timeout_loop: loop {
        /*
        if now.elapsed() >= TIMEOUT {
            return Err(anyhow::anyhow!("Command \"{}\" timed out when waiting for \"{}\"", &command.send, &command.expect));
        }*/

        // Cause we never know
        sleep(Duration::from_millis(100));

        let expect_err = telnet.exp_string(&command.expect);

        if let Ok(buffer) = expect_err {
            trace!(target: "rx", "{}", buffer);
            break 'timeout_loop;
        }
        else if command.can_fail {
            trace_telnet_output(telnet)?;
            break 'timeout_loop;
        }
        else {
            trace_telnet_output(telnet)?;

            if let Some(remote_stop) = &remote_stop && remote_stop.load(Ordering::Relaxed) == true {
                trace!(target: TARGET, "remote stop");
                break 'timeout_loop;
            }

            continue 'timeout_loop;
        }
    }

    Ok(())
}

fn send_command(telnet: &mut PtySession, command: &OsCommand, remote_stop: &Option<Arc<AtomicBool>>) -> anyhow::Result<()> {
    let now = Instant::now();

    //println!("{:?}", &command);

    match &command.send {
        SendType::NewLine => {
            let _ = telnet.send_line("");
        },
        SendType::Text(text, new_line) => match *new_line {
            true => {
                let _ = telnet.send_line(&text);
            }
            false => {
                let _ = telnet.send(&text);
                let _ = telnet.flush();
            }
        },
        SendType::Ctrl(char) => {
            let _ = telnet.send_control(*char);
        },
        SendType::Wait(time_ms) => {
            let to_wait = Duration::from_millis(*time_ms);

            'wait_loop: while now.elapsed() < to_wait {
                trace_telnet_output(telnet)?;
                sleep(Duration::from_millis(100));

                if let Some(remote_stop) = &remote_stop && remote_stop.load(Ordering::Relaxed) == true {
                    trace!(target: TARGET, "remote stop");
                    break 'wait_loop;
                }
            }
        }
    };

    Ok(())
}

fn trace_telnet_output(telnet: &mut PtySession) -> anyhow::Result<()> {
    match telnet.read_line() {
        Ok(text) => {
            trace!(target: "rx", "{}", text);
            Ok(())
        }
        Err(error) => match error {
            Error::Timeout { .. } => Ok(()),
            _ => Err(anyhow!(error)),
        }
    }
}