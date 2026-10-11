//! `spawn:<name>`: the transport starts a process on a channel it owns. The
//! process takes the channel as fd 3, or dials a socket path back; the
//! channel's end says how the process ended, and closing it ends the process.
#![cfg(unix)]
use rutis_bridge::channel::PeerId;
use rutis_bridge::transport::local::{Handover, LocalTransport, Spawn};
use rutis_bridge::{ConnectError, Dial, Transport};

fn shell(script: &str) -> Spawn {
    let mut spawn = Spawn::new("sh", PeerId::new("child").unwrap());
    // `sh -c script <channel> <trailing>`: the channel is $0, the rest $1….
    spawn.args = vec!["-c".into(), script.into()];
    spawn.trailing = vec!["extra".into()];
    spawn
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spawned_process_talks_on_fd_3_and_its_exit_ends_the_channel() {
    let transport = LocalTransport::default();
    // Answers one line with the line and its arguments, then exits 7.
    transport.spawner(
        "echo",
        shell(r#"read line <&3; echo "$line $0 $1" >&3; exit 7"#),
    );
    let mut channel = transport.dial(&Dial::address("spawn:echo")).await.unwrap();
    assert_eq!(channel.info.transport, "fd");
    assert_eq!(channel.info.peer, Some(PeerId::new("child").unwrap()));

    channel.sender.send(b"hello").unwrap();
    let reply = tokio::task::spawn_blocking(move || {
        let reply = channel.receiver.recv().unwrap().unwrap();
        (reply, channel.receiver.recv())
    })
    .await
    .unwrap();
    assert_eq!(reply.0, b"hello fd:3 extra");
    match reply.1 {
        Err(rutis_bridge::channel::ChannelError::Closed { reason }) => {
            assert_eq!(reason, "the process exited with exit status: 7")
        }
        other => panic!("the exit should end the channel, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_channel_ends_the_process() {
    let transport = LocalTransport::default();
    transport.spawner("idle", shell("exec sleep 30"));
    let mut channel = transport.dial(&Dial::address("spawn:idle")).await.unwrap();
    transport.close_all();
    // It does not end by itself: killed after the grace period, and the
    // channel's end says so.
    let end = tokio::task::spawn_blocking(move || channel.receiver.recv())
        .await
        .unwrap();
    match end {
        Err(rutis_bridge::channel::ChannelError::Closed { reason }) => {
            assert!(reason.contains("signal: 9"), "{reason}")
        }
        other => panic!("closing should end the process, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_process_without_fd_3_dials_back() {
    let transport = LocalTransport::default();
    let mut spawn = Spawn::new("python3", PeerId::new("child").unwrap());
    spawn.args = vec![
        "-c".into(),
        "import socket, sys\n\
         s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])\n\
         s.sendall(b'from ' + sys.argv[2].encode() + b'\\n')\n\
         s.recv(1)"
            .into(),
    ];
    spawn.handover = Handover::DialBack;
    spawn.trailing = vec!["anchor".into()];
    transport.spawner("back", spawn);
    let mut channel = transport.dial(&Dial::address("spawn:back")).await.unwrap();
    assert_eq!(channel.info.transport, "unix");
    let message = tokio::task::spawn_blocking(move || channel.receiver.recv())
        .await
        .unwrap();
    assert_eq!(message.unwrap().unwrap(), b"from anchor");
}

#[tokio::test]
async fn what_cannot_start_is_incompatible() {
    let transport = LocalTransport::default();
    assert!(matches!(
        transport.dial(&Dial::address("spawn:none")).await,
        Err(ConnectError::Incompatible { .. })
    ));
    transport.spawner(
        "missing",
        Spawn::new("/nonexistent/program", PeerId::new("child").unwrap()),
    );
    assert!(matches!(
        transport.dial(&Dial::address("spawn:missing")).await,
        Err(ConnectError::Incompatible { .. })
    ));
}

/// #246: a process may close its channel well before it exits, longer than
/// the grace period under load. The channel's end, and a send after it,
/// wait for the exit however long it takes, and say how the process ended.
#[tokio::test(flavor = "multi_thread")]
async fn the_end_of_the_channel_waits_for_the_exit_status() {
    let directory = tempfile::tempdir().unwrap();
    let gate = directory.path().join("gate");
    let closed = directory.path().join("closed");
    assert!(std::process::Command::new("mkfifo")
        .arg(&gate)
        .status()
        .unwrap()
        .success());
    let transport = LocalTransport::default();
    let mut spawn = shell(r#"exec 3>&-; : > "$CLOSED"; read line < "$GATE"; exit 17"#);
    spawn.env = vec![
        ("GATE".into(), gate.clone().into()),
        ("CLOSED".into(), closed.clone().into()),
    ];
    transport.spawner("slow", spawn);
    let channel = transport.dial(&Dial::address("spawn:slow")).await.unwrap();
    let (mut sender, mut receiver) = (channel.sender, channel.receiver);
    while !closed.exists() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let (received, receiving) = std::sync::mpsc::channel();
    std::thread::spawn(move || received.send(receiver.recv().map(|_| ())).unwrap());
    let (sent, sending) = std::sync::mpsc::channel();
    std::thread::spawn(move || sent.send(sender.send(b"{}")).unwrap());

    // Past the grace period and the second the channel's end used to wait
    // for: still waiting, since the process is still running.
    let past = std::time::Duration::from_millis(3500);
    assert!(
        receiving.recv_timeout(past).is_err(),
        "the channel ended first"
    );
    assert!(sending.try_recv().is_err(), "the send failed first");

    std::fs::write(&gate, b"exit\n").unwrap();
    for (end, ended) in [
        ("recv", receiving.recv().unwrap()),
        ("send", sending.recv().unwrap()),
    ] {
        match ended {
            Err(rutis_bridge::channel::ChannelError::Closed { reason }) => {
                assert_eq!(reason, "the process exited with exit status: 17", "{end}")
            }
            other => panic!("the {end} should fail with the exit status, got {other:?}"),
        }
    }
}
