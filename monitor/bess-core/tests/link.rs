//! Link supervisor (D5 §2–3): every transition of the state diagram with its
//! trigger and its recovery, CONFIG answers, the heartbeat identity, the
//! timing taken from CONFIG and the rate limit on state-change events.

mod common;

use bess_core::link::{
    CONFIG_REQUEST_PERIOD, DEFAULT_DATA_TIMEOUT, FLAP_WINDOW, LinkEvent, LinkState, LinkSupervisor,
    Output,
};
use common::{BOOT1, BOOT1_REV2, BOOT2, config_with, ms, telemetry_frame};
use protocol::config::{Config, FirmwareVersion, Rejection, Violation};
use protocol::messages::{
    AckStatus, ConfigAck, Heartbeat, Identity, Message, ResetAck, ResetResult,
};
use std::time::Instant;

fn config_msg(config: &Config) -> Message {
    Message::Config(Box::new(config.to_bytes()))
}

fn valid_config(identity: Identity) -> Message {
    config_msg(&config_with(identity, |_| {}))
}

fn telemetry(identity: Identity) -> Message {
    Message::Telemetry(telemetry_frame(identity))
}

fn sent(out: &[Output]) -> Vec<&Message> {
    out.iter()
        .filter_map(|o| match o {
            Output::Send(m) => Some(m),
            Output::Event(_) => None,
        })
        .collect()
}

fn events(out: &[Output]) -> Vec<&LinkEvent> {
    out.iter()
        .filter_map(|o| match o {
            Output::Event(e) => Some(e),
            Output::Send(_) => None,
        })
        .collect()
}

fn ack(identity: Identity, status: AckStatus) -> Message {
    Message::ConfigAck(ConfigAck {
        boot_counter: identity.boot_counter,
        config_revision: identity.config_revision,
        status,
    })
}

fn heartbeat(identity: Identity) -> Message {
    Message::Heartbeat(Heartbeat {
        accepted_boot_counter: identity.boot_counter,
        accepted_config_revision: identity.config_revision,
    })
}

fn change(from: LinkState, to: LinkState) -> LinkEvent {
    LinkEvent::StateChange { from, to }
}

/// A supervisor taken ONLINE with the reference CONFIG for `BOOT1`, more
/// than [`FLAP_WINDOW`] ago so the next state change is logged at once.
/// Returns it with the time of the last frame.
fn online() -> (LinkSupervisor, Instant) {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.on_message(t0, &telemetry(BOOT1));
    link.on_message(t0 + ms(100), &valid_config(BOOT1));
    assert_eq!(link.state(), LinkState::Online);
    let mut now = t0;
    while now.duration_since(t0) <= FLAP_WINDOW + ms(1000) {
        now += ms(1000);
        link.on_message(now, &telemetry(BOOT1));
        link.poll(now);
    }
    (link, now)
}

// --- Start-up and OFFLINE -------------------------------------------------

#[test]
fn starts_offline_without_config_and_sends_identity_zero_heartbeat() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    assert_eq!(link.state(), LinkState::Offline);
    assert!(link.config().is_none());
    assert!(!link.shows_values());
    assert!(!link.shows_faults());
    assert_eq!(link.heartbeat(), Heartbeat::WAITING);
    assert_eq!(link.data_timeout(), DEFAULT_DATA_TIMEOUT);
    assert_eq!(link.next_deadline(), t0);
    let out = link.poll(t0);
    assert_eq!(
        out,
        vec![Output::Send(Message::Heartbeat(Heartbeat::WAITING))]
    );
    // Offline has no timeout and sends no CONFIG_REQUEST.
    assert_eq!(link.next_deadline(), t0 + ms(1000));
    assert_eq!(sent(&link.poll(t0 + ms(999))).len(), 0);
}

#[test]
fn first_telemetry_enters_wait_config_and_requests_config() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    let out = link.on_message(t0, &telemetry(BOOT1));
    assert_eq!(link.state(), LinkState::WaitConfig);
    assert_eq!(
        out,
        vec![
            Output::Event(change(LinkState::Offline, LinkState::WaitConfig)),
            Output::Send(Message::ConfigRequest),
        ]
    );
    assert_eq!(link.identity(), Some(BOOT1));
    // DD-69: faults are used, measurements are not (S2.4).
    assert!(link.shows_faults());
    assert!(!link.shows_values());
}

#[test]
fn fault_update_also_carries_the_identity() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.on_message(t0, &Message::FaultUpdate(telemetry_frame(BOOT2)));
    assert_eq!(link.state(), LinkState::WaitConfig);
    assert_eq!(link.identity(), Some(BOOT2));
}

#[test]
fn frames_without_identity_do_not_leave_offline() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    let reset_ack = Message::ResetAck(ResetAck {
        request_id: 7,
        boot_counter: 1,
        config_revision: 1,
        result: ResetResult::Accepted,
    });
    assert!(link.on_message(t0, &reset_ack).is_empty());
    // A CONFIG before any TELEMETRY has nothing to be checked against.
    assert!(link.on_message(t0, &valid_config(BOOT1)).is_empty());
    assert_eq!(link.state(), LinkState::Offline);
    assert!(link.config().is_none());
}

#[test]
fn pi_to_mcu_messages_are_ignored() {
    let (mut link, now) = online();
    for m in [
        heartbeat(BOOT1),
        ack(BOOT1, AckStatus::Accepted),
        Message::ConfigRequest,
    ] {
        assert!(link.on_message(now + ms(2000), &m).is_empty());
    }
    // They did not restart the timeout.
    link.poll(now + DEFAULT_DATA_TIMEOUT);
    assert_eq!(link.state(), LinkState::Offline);
}

// --- WAIT_CONFIG ----------------------------------------------------------

#[test]
fn config_request_repeats_every_second_while_waiting() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.poll(t0); // first heartbeat
    link.on_message(t0, &telemetry(BOOT1));
    assert_eq!(link.next_deadline(), t0 + CONFIG_REQUEST_PERIOD);
    assert!(!sent(&link.poll(t0 + ms(999))).contains(&&Message::ConfigRequest));
    link.on_message(t0 + ms(950), &telemetry(BOOT1));
    let out = link.poll(t0 + ms(1000));
    assert!(sent(&out).contains(&&Message::ConfigRequest));
    link.on_message(t0 + ms(1950), &telemetry(BOOT1));
    let out = link.poll(t0 + ms(2000));
    assert!(sent(&out).contains(&&Message::ConfigRequest));
    assert_eq!(link.state(), LinkState::WaitConfig);
}

#[test]
fn valid_config_is_acknowledged_installed_and_goes_online() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.on_message(t0, &telemetry(BOOT1));
    let out = link.on_message(t0 + ms(100), &valid_config(BOOT1));
    let config = config_with(BOOT1, |_| {});
    assert_eq!(
        out,
        vec![
            Output::Send(ack(BOOT1, AckStatus::Accepted)),
            Output::Event(LinkEvent::ConfigAccepted(Box::new(config.clone()))),
            // Heartbeat with the new identity at once, so the MCU stops
            // inhibiting charging without waiting for the next period.
            Output::Send(heartbeat(BOOT1)),
            // A different change from OFFLINE -> WAIT_CONFIG: logged at once.
            Output::Event(change(LinkState::WaitConfig, LinkState::Online)),
        ]
    );
    assert_eq!(link.state(), LinkState::Online);
    assert_eq!(link.config(), Some(&config));
    assert_eq!(
        link.heartbeat(),
        Heartbeat::from_bytes(&[1, 0, 0, 0, 1, 0, 0, 0])
    );
    assert!(link.shows_values());
    // No more CONFIG_REQUEST once ONLINE.
    link.on_message(t0 + ms(900), &telemetry(BOOT1));
    assert!(!sent(&link.poll(t0 + ms(1100))).contains(&&Message::ConfigRequest));
}

#[test]
fn invalid_content_is_rejected_every_time_but_logged_once() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.on_message(t0, &telemetry(BOOT1));
    let bad = config_with(BOOT1, |c| c.capacity_mah = 0);
    let out = link.on_message(t0 + ms(100), &config_msg(&bad));
    assert_eq!(
        out,
        vec![
            Output::Send(ack(BOOT1, AckStatus::InvalidContent)),
            Output::Event(LinkEvent::ConfigRejected {
                config: Box::new(bad.clone()),
                rejection: Rejection::InvalidContent(vec![Violation::ZeroCapacity]),
            }),
        ]
    );
    assert_eq!(link.state(), LinkState::WaitConfig);
    assert!(link.config().is_none());
    // Asked again, answered again, not logged again.
    link.on_message(t0 + ms(900), &telemetry(BOOT1));
    assert!(sent(&link.poll(t0 + ms(1100))).contains(&&Message::ConfigRequest));
    let out = link.on_message(t0 + ms(1200), &config_msg(&bad));
    assert_eq!(
        out,
        vec![Output::Send(ack(BOOT1, AckStatus::InvalidContent))]
    );
    // Recovery: a valid CONFIG with a new revision is accepted.
    link.on_message(t0 + ms(1900), &telemetry(BOOT1_REV2));
    let out = link.on_message(t0 + ms(2000), &valid_config(BOOT1_REV2));
    assert_eq!(sent(&out)[0], &ack(BOOT1_REV2, AckStatus::Accepted));
    assert_eq!(link.state(), LinkState::Online);
}

#[test]
fn unsupported_schema_gets_status_1() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.on_message(t0, &telemetry(BOOT1));
    let bad = config_with(BOOT1, |c| c.schema = 2);
    let out = link.on_message(t0, &config_msg(&bad));
    assert_eq!(sent(&out), vec![&ack(BOOT1, AckStatus::UnsupportedSchema)]);
    assert_eq!(
        events(&out),
        vec![&LinkEvent::ConfigRejected {
            config: Box::new(bad),
            rejection: Rejection::UnsupportedSchema(2),
        }]
    );
    assert_eq!(link.state(), LinkState::WaitConfig);
}

#[test]
fn config_for_another_identity_gets_status_3() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.on_message(t0, &telemetry(BOOT1));
    let out = link.on_message(t0, &valid_config(BOOT2));
    assert_eq!(sent(&out), vec![&ack(BOOT2, AckStatus::IdentityMismatch)]);
    assert!(matches!(
        events(&out)[..],
        [LinkEvent::ConfigRejected {
            rejection: Rejection::IdentityMismatch { .. },
            ..
        }]
    ));
    assert_eq!(link.state(), LinkState::WaitConfig);
}

#[test]
fn wait_config_times_out_to_offline_and_recovers() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.on_message(t0, &telemetry(BOOT1));
    link.poll(t0 + ms(2999));
    assert_eq!(link.state(), LinkState::WaitConfig);
    link.poll(t0 + ms(3000));
    assert_eq!(link.state(), LinkState::Offline);
    link.on_message(t0 + ms(4000), &telemetry(BOOT1));
    assert_eq!(link.state(), LinkState::WaitConfig);
}

// --- ONLINE and the link timeout (S8.12) ----------------------------------

#[test]
fn online_goes_offline_3_s_after_the_last_valid_frame() {
    let (mut link, last) = online();
    assert_eq!(
        link.next_deadline().min(last + ms(3000)),
        link.next_deadline()
    );
    link.poll(last + ms(2999));
    assert_eq!(link.state(), LinkState::Online);
    let out = link.poll(last + ms(3000));
    assert_eq!(link.state(), LinkState::Offline);
    assert_eq!(
        events(&out),
        vec![&change(LinkState::Online, LinkState::Offline)]
    );
    assert!(!link.shows_values());
    assert!(!link.shows_faults());
    // The CONFIG is kept for a quick resume (DD-27), and so is its
    // identity in the heartbeat.
    assert!(link.config().is_some());
    assert_eq!(link.heartbeat().accepted_boot_counter, 1);
}

#[test]
fn any_valid_mcu_frame_restarts_the_timeout() {
    let (mut link, last) = online();
    let reset_ack = Message::ResetAck(ResetAck {
        request_id: 1,
        boot_counter: 1,
        config_revision: 1,
        result: ResetResult::Accepted,
    });
    link.on_message(last + ms(2500), &reset_ack);
    link.poll(last + ms(5000));
    assert_eq!(link.state(), LinkState::Online);
    link.poll(last + ms(5500));
    assert_eq!(link.state(), LinkState::Offline);
}

#[test]
fn offline_with_unchanged_identity_resumes_online_without_rereading() {
    let (mut link, last) = online();
    link.poll(last + ms(3000));
    assert_eq!(link.state(), LinkState::Offline);
    let out = link.on_message(last + ms(20_000), &telemetry(BOOT1));
    assert_eq!(link.state(), LinkState::Online);
    assert!(link.shows_values());
    assert_eq!(
        out,
        vec![Output::Event(change(LinkState::Offline, LinkState::Online))]
    );
}

#[test]
fn heartbeat_keeps_going_while_offline() {
    let (mut link, last) = online();
    link.poll(last + ms(3000));
    let out = link.poll(last + ms(4000));
    assert!(sent(&out).contains(&&heartbeat(BOOT1)));
}

// --- Identity changes (DD-27, DD-55) --------------------------------------

#[test]
fn mcu_restart_drops_config_and_waits_for_the_new_one() {
    let (mut link, last) = online();
    let now = last + ms(1000);
    let out = link.on_message(now, &telemetry(BOOT2));
    assert_eq!(
        out,
        vec![
            Output::Event(LinkEvent::McuRestart {
                previous: BOOT1,
                current: BOOT2,
            }),
            Output::Event(change(LinkState::Online, LinkState::WaitConfig)),
            Output::Send(Message::ConfigRequest),
        ]
    );
    // This frame's values belong to an unknown CONFIG: not shown (S2.4).
    assert!(!link.shows_values());
    assert!(link.config().is_none()); // fans to 100 % (DD-55)
    assert_eq!(link.heartbeat(), Heartbeat::WAITING);
    assert_eq!(link.data_timeout(), DEFAULT_DATA_TIMEOUT);
    // Recovery.
    link.on_message(now + ms(100), &valid_config(BOOT2));
    assert_eq!(link.state(), LinkState::Online);
    assert_eq!(link.heartbeat().accepted_boot_counter, 2);
}

#[test]
fn new_revision_is_a_config_change() {
    let (mut link, last) = online();
    let out = link.on_message(last + ms(1000), &telemetry(BOOT1_REV2));
    assert_eq!(
        events(&out)[0],
        &LinkEvent::ConfigChange {
            previous: BOOT1,
            current: BOOT1_REV2,
        }
    );
    assert_eq!(link.state(), LinkState::WaitConfig);
}

#[test]
fn restart_while_offline_goes_to_wait_config() {
    let (mut link, last) = online();
    link.poll(last + ms(3000));
    let out = link.on_message(last + ms(20_000), &telemetry(BOOT2));
    assert_eq!(
        events(&out),
        vec![
            &LinkEvent::McuRestart {
                previous: BOOT1,
                current: BOOT2,
            },
            &change(LinkState::Offline, LinkState::WaitConfig),
        ]
    );
    assert!(link.config().is_none());
}

#[test]
fn restart_during_wait_config_is_logged_too() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.on_message(t0, &telemetry(BOOT1));
    let out = link.on_message(t0 + ms(1000), &telemetry(BOOT2));
    assert_eq!(
        events(&out),
        vec![&LinkEvent::McuRestart {
            previous: BOOT1,
            current: BOOT2,
        }]
    );
    assert_eq!(link.state(), LinkState::WaitConfig);
}

#[test]
fn firmware_change_spans_a_pi_restart_when_seeded() {
    let t0 = Instant::now();
    let old = FirmwareVersion {
        major: 0,
        minor: 9,
        patch: 1,
    };
    let mut link = LinkSupervisor::new(t0).with_last_firmware(Some(old));
    link.on_message(t0, &telemetry(BOOT1));
    let out = link.on_message(t0 + ms(100), &valid_config(BOOT1));
    assert_eq!(
        events(&out)[0],
        &LinkEvent::FirmwareChange {
            previous: old,
            current: FirmwareVersion {
                major: 0,
                minor: 0,
                patch: 0,
            },
        }
    );
    // Without a seed the first CONFIG of a run is not a change.
    let mut link = LinkSupervisor::new(t0).with_last_firmware(None);
    link.on_message(t0, &telemetry(BOOT1));
    let out = link.on_message(t0 + ms(100), &valid_config(BOOT1));
    assert!(
        !events(&out)
            .iter()
            .any(|e| matches!(e, LinkEvent::FirmwareChange { .. }))
    );
}

#[test]
fn firmware_change_is_logged_on_the_next_accepted_config() {
    let (mut link, last) = online();
    link.on_message(last + ms(1000), &telemetry(BOOT2));
    let newer = config_with(BOOT2, |c| {
        c.firmware = FirmwareVersion {
            major: 1,
            minor: 2,
            patch: 0,
        };
    });
    let out = link.on_message(last + ms(1100), &config_msg(&newer));
    assert_eq!(
        events(&out)[0],
        &LinkEvent::FirmwareChange {
            previous: FirmwareVersion {
                major: 0,
                minor: 0,
                patch: 0
            },
            current: newer.firmware,
        }
    );
}

#[test]
fn same_firmware_after_restart_is_not_a_firmware_change() {
    let (mut link, last) = online();
    link.on_message(last + ms(1000), &telemetry(BOOT2));
    let out = link.on_message(last + ms(1100), &valid_config(BOOT2));
    assert!(
        !events(&out)
            .iter()
            .any(|e| matches!(e, LinkEvent::FirmwareChange { .. }))
    );
}

// --- CONFIG outside WAIT_CONFIG -------------------------------------------

#[test]
fn repeated_config_while_online_is_acknowledged_again_without_events() {
    let (mut link, last) = online();
    let out = link.on_message(last + ms(500), &valid_config(BOOT1));
    assert_eq!(out, vec![Output::Send(ack(BOOT1, AckStatus::Accepted))]);
    assert_eq!(link.state(), LinkState::Online);
}

#[test]
fn unsolicited_config_of_another_identity_is_not_answered() {
    // The MCU sends a CONFIG on its own after start-up (§8); after a quick
    // restart it can come before the first TELEMETRY of the new boot.
    let (mut link, last) = online();
    assert!(
        link.on_message(last + ms(500), &valid_config(BOOT2))
            .is_empty()
    );
    assert_eq!(link.state(), LinkState::Online);
    assert_eq!(link.config().map(Config::identity), Some(BOOT1));
}

#[test]
fn config_while_offline_is_not_answered() {
    let (mut link, last) = online();
    link.poll(last + ms(3000));
    assert!(
        link.on_message(last + ms(4000), &valid_config(BOOT2))
            .is_empty()
    );
    assert_eq!(link.state(), LinkState::Offline);
}

// --- Timing from CONFIG (DD-24) -------------------------------------------

#[test]
fn timeout_and_heartbeat_period_come_from_the_installed_config() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.poll(t0);
    link.on_message(t0, &telemetry(BOOT1));
    let config = config_with(BOOT1, |c| {
        c.pi_data_timeout_ms = 5000;
        c.heartbeat_ms = 500;
    });
    let out = link.on_message(t0 + ms(100), &config_msg(&config));
    assert!(sent(&out).contains(&&heartbeat(BOOT1)));
    assert_eq!(link.data_timeout(), ms(5000));
    assert_eq!(link.heartbeat_period(), ms(500));
    assert!(sent(&link.poll(t0 + ms(600))).contains(&&heartbeat(BOOT1)));
    // The last valid frame was the CONFIG at t0 + 100 ms.
    link.poll(t0 + ms(3100));
    assert_eq!(link.state(), LinkState::Online);
    link.poll(t0 + ms(5099));
    assert_eq!(link.state(), LinkState::Online);
    link.poll(t0 + ms(5100));
    assert_eq!(link.state(), LinkState::Offline);
}

#[test]
fn a_late_poll_sends_one_heartbeat_not_a_burst() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.poll(t0);
    let out = link.poll(t0 + ms(5500));
    assert_eq!(sent(&out), vec![&Message::Heartbeat(Heartbeat::WAITING)]);
    assert_eq!(link.next_deadline(), t0 + ms(6500));
}

// --- State-change events, at most one per 10 s (DD-80) --------------------

#[test]
fn normal_start_up_logs_every_change() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    let mut out = link.on_message(t0, &telemetry(BOOT1));
    out.extend(link.on_message(t0 + ms(100), &valid_config(BOOT1)));
    let changes: Vec<_> = events(&out)
        .into_iter()
        .filter(|e| matches!(e, LinkEvent::StateChange { .. }))
        .collect();
    assert_eq!(
        changes,
        vec![
            &change(LinkState::Offline, LinkState::WaitConfig),
            &change(LinkState::WaitConfig, LinkState::Online),
        ]
    );
}

#[test]
fn flapping_link_is_logged_once_with_a_count() {
    let (mut link, last) = online();
    // A loose connector: frames stop and come back three times.
    let mut logged = Vec::new();
    let mut now = last;
    for _ in 0..3 {
        now += ms(3000);
        logged.extend(link.poll(now));
        now += ms(500);
        logged.extend(link.on_message(now, &telemetry(BOOT1)));
    }
    // Each kind of change is logged once right away; the repeats are counted.
    assert_eq!(
        events(&logged),
        vec![
            &change(LinkState::Online, LinkState::Offline),
            &change(LinkState::Offline, LinkState::Online),
        ]
    );
    // Frames keep coming from now on (last one at + 10.5 s).
    link.on_message(last + ms(12_500), &telemetry(BOOT1));
    // Each window closes 10 s after its first log (last + 13 s, + 13.5 s).
    assert_eq!(
        link.next_deadline().min(last + ms(13_000)),
        link.next_deadline()
    );
    let mut out = link.poll(last + ms(13_000));
    out.extend(link.poll(last + ms(13_500)));
    assert_eq!(
        events(&out),
        vec![
            &LinkEvent::Repeated {
                from: LinkState::Online,
                to: LinkState::Offline,
                count: 2,
            },
            &LinkEvent::Repeated {
                from: LinkState::Offline,
                to: LinkState::Online,
                count: 2,
            },
        ]
    );
    // The next windows stay quiet and close without an event; after that a
    // change is logged at once again.
    for i in 1..=10 {
        let t = last + ms(13_500 + 1000 * i);
        link.on_message(t, &telemetry(BOOT1));
        assert!(events(&link.poll(t)).is_empty());
    }
    let out = link.poll(last + ms(26_500));
    assert_eq!(
        events(&out),
        vec![&change(LinkState::Online, LinkState::Offline)]
    );
}

#[test]
fn next_deadline_covers_timeout_request_and_flap_window() {
    let t0 = Instant::now();
    let mut link = LinkSupervisor::new(t0);
    link.poll(t0); // heartbeat next due at t0 + 1 s
    link.on_message(t0 + ms(200), &telemetry(BOOT1));
    // Request at t0 + 1.2 s, heartbeat at t0 + 1 s.
    assert_eq!(link.next_deadline(), t0 + ms(1000));
    link.poll(t0 + ms(1000));
    assert_eq!(link.next_deadline(), t0 + ms(1200));
    link.poll(t0 + ms(1200));
    // Heartbeat t0 + 2 s, request t0 + 2.2 s, timeout t0 + 3.2 s.
    assert_eq!(link.next_deadline(), t0 + ms(2000));
}
