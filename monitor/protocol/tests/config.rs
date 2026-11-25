//! CONFIG (UART draft §6–7): the reference CONFIG, every field offset, a
//! lossless round trip, the check order of §8, each §7 content rule with its
//! boundaries, and the description-text rules that are deliberately not
//! checked (DD-67).

mod common;

use common::{CONFIG, TELEMETRY_CORRECTED, hex};
use protocol::config::{
    Config, ConfigFlags, CurrentProfile, FanOfflineAction, FanPoint, FanTable, FirmwareVersion,
    Rejection, TABLE_SLOTS, Table, Violation,
};
use protocol::frame::Direction;
use protocol::messages::{AckStatus, CONFIG_LEN, ConfigAck, Identity, Message, Telemetry};
use protocol::parser::FrameParser;

const REFERENCE_IDENTITY: Identity = Identity {
    boot_counter: 1,
    config_revision: 1,
};

fn reference_bytes() -> [u8; CONFIG_LEN] {
    hex(CONFIG)[6..6 + CONFIG_LEN]
        .try_into()
        .expect("CONFIG payload is 190 bytes")
}

fn reference() -> Config {
    Config::from_bytes(&reference_bytes())
}

fn point(temperature_deci_c: i16, duty_pct: u8) -> FanPoint {
    FanPoint {
        temperature_deci_c,
        duty_pct,
        expected_rpm: 0,
    }
}

fn table(active: &[FanPoint]) -> FanTable {
    let mut points = [FanPoint::default(); TABLE_SLOTS];
    points[..active.len()].copy_from_slice(active);
    FanTable {
        count: u8::try_from(active.len()).expect("at most 8 points"),
        points,
    }
}

/// The §6–7 example values, written out field by field.
fn expected_reference() -> Config {
    Config {
        schema: 1,
        config_revision: 1,
        boot_counter: 1,
        firmware: FirmwareVersion {
            major: 0,
            minor: 0,
            patch: 0,
        },
        chemistry: 1,
        series_groups: 4,
        parallel_cells: 3,
        ntc_present_mask: 7,
        current_profile: CurrentProfile::Today,
        capacity_mah: 9600,
        flags: ConfigFlags(0),
        ov_trip_mv: 3600,
        ov_recover_mv: 3450,
        uv_trip_mv: 2700,
        uv_recover_mv: 2900,
        chg_oc_ma: 9000,
        dis_oc_ma: 6500,
        dis_overload_ma: 5500,
        dis_peak_ma: 5500,
        chg_ut_trip_deci_c: 50,
        chg_ut_recover_deci_c: 100,
        chg_ot_trip_deci_c: 500,
        chg_ot_recover_deci_c: 450,
        dis_ut_trip_deci_c: -100,
        dis_ut_recover_deci_c: -50,
        dis_ot_trip_deci_c: 550,
        dis_ot_recover_deci_c: 500,
        terminal_ot_deci_c: 700,
        terminal_delta_deci_c: 150,
        air_ot_deci_c: 450,
        air_recover_deci_c: 400,
        voltage_detect_ms: 1000,
        temp_detect_ms: 1000,
        recover_ms: 5000,
        overload_ms: 10000,
        soc_min_centi_pct: 2000,
        soc_max_centi_pct: 8500,
        battery_fan_on_deci_c: 300,
        battery_fan_off_deci_c: 270,
        air_fan_on_deci_c: 350,
        air_fan_off_deci_c: 320,
        fan_min_pct: 30,
        fan_speed_min_pct: 70,
        fan_fault_ms: 5000,
        fan_offline_ms: 65535,
        telemetry_ms: 1000,
        heartbeat_ms: 1000,
        mcu_pi_timeout_ms: 5000,
        pi_data_timeout_ms: 3000,
        battery_table: table(&[
            point(270, 0),
            point(300, 30),
            point(400, 65),
            point(450, 100),
        ]),
        air_table: table(&[
            point(320, 0),
            point(350, 30),
            point(400, 65),
            point(450, 100),
        ]),
        fan_offline_action: FanOfflineAction::Undecided,
        reserved: 0,
    }
}

#[test]
fn reference_config_decodes_to_the_documented_values() {
    assert_eq!(reference(), expected_reference());
}

#[test]
fn reference_config_re_encodes_byte_for_byte() {
    assert_eq!(expected_reference().to_bytes(), reference_bytes());
}

#[test]
fn reference_frame_decodes_through_the_parser_and_re_encodes() {
    let frame = hex(CONFIG);
    let mut parser = FrameParser::new(Direction::McuToPi);
    let frames = parser.push(&frame);
    assert_eq!(frames.len(), 1);
    let Message::Config(raw) = Message::try_from(&frames[0]).expect("valid") else {
        panic!("expected CONFIG");
    };
    let config = Config::from_bytes(&raw);
    assert_eq!(config.check(REFERENCE_IDENTITY), Ok(()));
    assert_eq!(
        Message::Config(Box::new(config.to_bytes())).encode(0),
        frame
    );
}

/// Bytes 0, 1, 2, … so every field's value shows its offset.
fn counting_bytes() -> [u8; CONFIG_LEN] {
    std::array::from_fn(|i| u8::try_from(i).expect("190 < 256"))
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le16s(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[test]
fn every_field_sits_at_its_section_6_offset() {
    let b = counting_bytes();
    let c = Config::from_bytes(&b);
    assert_eq!(c.schema, le16(&b, 0));
    assert_eq!(c.config_revision, le32(&b, 2));
    assert_eq!(c.boot_counter, le32(&b, 6));
    assert_eq!(
        c.firmware,
        FirmwareVersion {
            major: 10,
            minor: 11,
            patch: 12
        }
    );
    assert_eq!(
        [
            c.chemistry,
            c.series_groups,
            c.parallel_cells,
            c.ntc_present_mask
        ],
        [13, 14, 15, 16]
    );
    assert_eq!(c.current_profile, CurrentProfile::Unknown(17));
    assert_eq!(c.capacity_mah, le32(&b, 18));
    assert_eq!(c.flags, ConfigFlags(le16(&b, 22)));
    assert_eq!(
        [c.ov_trip_mv, c.ov_recover_mv, c.uv_trip_mv, c.uv_recover_mv],
        [le16(&b, 24), le16(&b, 26), le16(&b, 28), le16(&b, 30)]
    );
    assert_eq!(
        [c.chg_oc_ma, c.dis_oc_ma, c.dis_overload_ma, c.dis_peak_ma],
        [le32(&b, 32), le32(&b, 36), le32(&b, 40), le32(&b, 44)]
    );
    let temperatures = [
        c.chg_ut_trip_deci_c,
        c.chg_ut_recover_deci_c,
        c.chg_ot_trip_deci_c,
        c.chg_ot_recover_deci_c,
        c.dis_ut_trip_deci_c,
        c.dis_ut_recover_deci_c,
        c.dis_ot_trip_deci_c,
        c.dis_ot_recover_deci_c,
        c.terminal_ot_deci_c,
        c.terminal_delta_deci_c,
        c.air_ot_deci_c,
        c.air_recover_deci_c,
    ];
    let expected: Vec<i16> = (48..72).step_by(2).map(|at| le16s(&b, at)).collect();
    assert_eq!(temperatures.to_vec(), expected);
    assert_eq!(
        [
            c.voltage_detect_ms,
            c.temp_detect_ms,
            c.recover_ms,
            c.overload_ms
        ],
        [le16(&b, 72), le16(&b, 74), le16(&b, 76), le16(&b, 78)]
    );
    assert_eq!(
        [c.soc_min_centi_pct, c.soc_max_centi_pct],
        [le16(&b, 80), le16(&b, 82)]
    );
    assert_eq!(
        [
            c.battery_fan_on_deci_c,
            c.battery_fan_off_deci_c,
            c.air_fan_on_deci_c,
            c.air_fan_off_deci_c
        ],
        [le16s(&b, 84), le16s(&b, 86), le16s(&b, 88), le16s(&b, 90)]
    );
    assert_eq!([c.fan_min_pct, c.fan_speed_min_pct], [92, 93]);
    assert_eq!(
        [
            c.fan_fault_ms,
            c.fan_offline_ms,
            c.telemetry_ms,
            c.heartbeat_ms,
            c.mcu_pi_timeout_ms,
            c.pi_data_timeout_ms
        ],
        [
            le16(&b, 94),
            le16(&b, 96),
            le16(&b, 98),
            le16(&b, 100),
            le16(&b, 102),
            le16(&b, 104)
        ]
    );
    assert_eq!([c.battery_table.count, c.air_table.count], [106, 107]);
    assert_eq!(c.fan_offline_action, FanOfflineAction::Unknown(108));
    assert_eq!(c.reserved, 109);
}

#[test]
fn every_fan_point_sits_at_its_section_7_offset() {
    let b = counting_bytes();
    let c = Config::from_bytes(&b);
    for (table, base) in [(&c.battery_table, 110), (&c.air_table, 150)] {
        for (i, p) in table.points.iter().enumerate() {
            let at = base + 5 * i;
            assert_eq!(
                *p,
                FanPoint {
                    temperature_deci_c: le16s(&b, at),
                    duty_pct: b[at + 2],
                    expected_rpm: le16(&b, at + 3),
                },
                "point {i} at offset {at}"
            );
        }
    }
}

#[test]
fn decoding_keeps_every_byte() {
    let mut b = counting_bytes();
    assert_eq!(Config::from_bytes(&b).to_bytes(), b);
    // A few pseudo-random payloads (xorshift), so no byte value is special.
    let mut state: u32 = 0x1234_5678;
    for _ in 0..200 {
        for byte in &mut b {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state.to_le_bytes()[0];
        }
        assert_eq!(Config::from_bytes(&b).to_bytes(), b);
    }
}

#[test]
fn every_profile_and_offline_action_byte_round_trips() {
    for value in 0..=u8::MAX {
        assert_eq!(u8::from(CurrentProfile::from(value)), value);
        assert_eq!(u8::from(FanOfflineAction::from(value)), value);
    }
    assert_eq!(CurrentProfile::from(1), CurrentProfile::Upgrade);
    assert_eq!(FanOfflineAction::from(1), FanOfflineAction::HoldThenOff);
    assert_eq!(FanOfflineAction::from(2), FanOfflineAction::Full);
}

#[test]
fn reference_config_is_accepted_and_acked() {
    let c = reference();
    assert_eq!(c.violations(), vec![]);
    assert_eq!(c.check(REFERENCE_IDENTITY), Ok(()));
    assert_eq!(c.identity(), REFERENCE_IDENTITY);
    assert_eq!(
        c.ack(AckStatus::Accepted),
        ConfigAck {
            boot_counter: 1,
            config_revision: 1,
            status: AckStatus::Accepted
        }
    );
}

#[test]
fn reference_config_matches_the_reference_telemetry() {
    let frame = hex(TELEMETRY_CORRECTED);
    let payload = frame[6..frame.len() - 2].try_into().expect("38 bytes");
    let telemetry = Telemetry::from_bytes(payload);
    assert_eq!(telemetry.identity(), REFERENCE_IDENTITY);
    assert_eq!(reference().check(telemetry.identity()), Ok(()));
}

#[test]
fn schema_is_checked_first() {
    for schema in [0, 2, u16::MAX] {
        let mut c = reference();
        c.schema = schema;
        c.boot_counter = 99; // wrong identity too
        c.chemistry = 0; // and broken content
        let rejection = c.check(REFERENCE_IDENTITY).unwrap_err();
        assert_eq!(rejection, Rejection::UnsupportedSchema(schema));
        assert_eq!(rejection.status(), AckStatus::UnsupportedSchema);
    }
}

#[test]
fn identity_is_checked_before_content() {
    let mut c = reference();
    c.chemistry = 0;
    for telemetry in [
        Identity {
            boot_counter: 2,
            config_revision: 1,
        },
        Identity {
            boot_counter: 1,
            config_revision: 2,
        },
        Identity {
            boot_counter: 0,
            config_revision: 0,
        },
    ] {
        let rejection = c.check(telemetry).unwrap_err();
        assert_eq!(
            rejection,
            Rejection::IdentityMismatch {
                config: REFERENCE_IDENTITY,
                telemetry
            }
        );
        assert_eq!(rejection.status(), AckStatus::IdentityMismatch);
    }
}

#[test]
fn broken_content_lists_every_violation_in_list_order() {
    let mut c = reference();
    c.capacity_mah = 0;
    c.reserved = 5;
    c.telemetry_ms = 0;
    let rejection = c.check(REFERENCE_IDENTITY).unwrap_err();
    assert_eq!(rejection.status(), AckStatus::InvalidContent);
    assert_eq!(
        rejection,
        Rejection::InvalidContent(vec![
            Violation::ReservedByte(5),
            Violation::ZeroCapacity,
            Violation::ZeroPeriod("telemetry_ms"),
        ])
    );
    let text = rejection.to_string();
    for part in ["reserved byte 109", "capacity_mAh", "telemetry_ms"] {
        assert!(text.contains(part), "{text}");
    }
}

/// One change to the reference CONFIG and the exact violations it gives.
struct Case {
    name: &'static str,
    change: fn(&mut Config),
    expected: Vec<Violation>,
}

fn case(name: &'static str, change: fn(&mut Config), expected: Vec<Violation>) -> Case {
    Case {
        name,
        change,
        expected,
    }
}

fn run(cases: Vec<Case>) {
    for Case {
        name,
        change,
        expected,
    } in cases
    {
        let mut c = reference();
        change(&mut c);
        assert_eq!(c.violations(), expected, "{name}");
        if !expected.is_empty() {
            let rejection = c.check(REFERENCE_IDENTITY).unwrap_err();
            assert_eq!(rejection.status(), AckStatus::InvalidContent, "{name}");
            for v in &expected {
                assert!(!v.to_string().is_empty());
            }
        }
    }
}

#[test]
fn reserved_bits_and_bytes_must_be_zero() {
    use Table::{Air, Battery};
    run(vec![
        case(
            "flags bit 2",
            |c| c.flags = ConfigFlags(1 << 2),
            vec![Violation::ReservedFlags(4)],
        ),
        case(
            "flags bit 15",
            |c| c.flags = ConfigFlags(1 << 15),
            vec![Violation::ReservedFlags(0x8000)],
        ),
        case(
            "ntc mask bit 4",
            |c| c.ntc_present_mask = 0x17,
            vec![Violation::ReservedNtcBits(0x17)],
        ),
        case(
            "byte 109",
            |c| c.reserved = 1,
            vec![Violation::ReservedByte(1)],
        ),
        case(
            "battery slot 4 temperature",
            |c| c.battery_table.points[4].temperature_deci_c = 1,
            vec![Violation::InactivePointNotZero {
                table: Battery,
                index: 4,
            }],
        ),
        case(
            "air slot 7 rpm",
            |c| c.air_table.points[7].expected_rpm = 1,
            vec![Violation::InactivePointNotZero {
                table: Air,
                index: 7,
            }],
        ),
        case(
            "battery slot 5 duty",
            |c| c.battery_table.points[5].duty_pct = 1,
            vec![Violation::InactivePointNotZero {
                table: Battery,
                index: 5,
            }],
        ),
    ]);
}

#[test]
fn build_must_be_lfp_4s3p_with_ntc1_to_3() {
    run(vec![
        case(
            "chemistry 0",
            |c| c.chemistry = 0,
            vec![Violation::Chemistry(0)],
        ),
        case(
            "chemistry 2",
            |c| c.chemistry = 2,
            vec![Violation::Chemistry(2)],
        ),
        case(
            "3 groups",
            |c| c.series_groups = 3,
            vec![Violation::SeriesGroups(3)],
        ),
        case(
            "5 groups",
            |c| c.series_groups = 5,
            vec![Violation::SeriesGroups(5)],
        ),
        case(
            "2 parallel",
            |c| c.parallel_cells = 2,
            vec![Violation::ParallelCells(2)],
        ),
        case(
            "capacity 0",
            |c| c.capacity_mah = 0,
            vec![Violation::ZeroCapacity],
        ),
        case(
            "no NTC1",
            |c| c.ntc_present_mask = 0b0110,
            vec![Violation::MissingNtc(6)],
        ),
        case(
            "no NTC2",
            |c| c.ntc_present_mask = 0b0101,
            vec![Violation::MissingNtc(5)],
        ),
        case(
            "no NTC3",
            |c| c.ntc_present_mask = 0b0011,
            vec![Violation::MissingNtc(3)],
        ),
        case("NTC4 fitted too", |c| c.ntc_present_mask = 0b1111, vec![]),
    ]);
}

#[test]
fn point_count_must_be_2_to_8() {
    use Table::{Air, Battery};
    run(vec![
        // A bad count gives only that violation for the table.
        case(
            "battery count 0",
            |c| c.battery_table.count = 0,
            vec![Violation::PointCount {
                table: Battery,
                count: 0,
            }],
        ),
        case(
            "battery count 1",
            |c| c.battery_table.count = 1,
            vec![Violation::PointCount {
                table: Battery,
                count: 1,
            }],
        ),
        case(
            "air count 9",
            |c| c.air_table.count = 9,
            vec![Violation::PointCount {
                table: Air,
                count: 9,
            }],
        ),
        case(
            "two points",
            |c| c.battery_table = table(&[point(270, 0), point(450, 100)]),
            vec![],
        ),
        case(
            "eight points",
            |c| {
                c.air_table = table(&[
                    point(250, 0),
                    point(320, 0),
                    point(350, 30),
                    point(370, 40),
                    point(390, 50),
                    point(400, 65),
                    point(420, 80),
                    point(450, 100),
                ]);
            },
            vec![],
        ),
    ]);
}

#[test]
fn table_temperatures_strictly_increase_and_duties_never_fall() {
    use Table::{Air, Battery};
    run(vec![
        case(
            "equal temperatures",
            |c| c.battery_table.points[2].temperature_deci_c = 300,
            vec![Violation::TemperaturesNotIncreasing {
                table: Battery,
                index: 2,
            }],
        ),
        case(
            "falling temperature",
            |c| c.air_table.points[3].temperature_deci_c = 390,
            vec![Violation::TemperaturesNotIncreasing {
                table: Air,
                index: 3,
            }],
        ),
        case(
            "falling duty",
            |c| c.battery_table.points[1].duty_pct = 70,
            vec![Violation::DutyDecreasing {
                table: Battery,
                index: 2,
            }],
        ),
        case(
            "equal duties are fine",
            |c| c.battery_table.points[2].duty_pct = 30,
            vec![],
        ),
        case(
            "duty above 100",
            |c| c.air_table.points[3].duty_pct = 101,
            vec![
                Violation::DutyAbove100 {
                    table: Air,
                    index: 3,
                    duty: 101,
                },
                Violation::LastPointNot100 { table: Air },
            ],
        ),
        case(
            "point below -300",
            |c| c.battery_table.points[0].temperature_deci_c = -301,
            vec![Violation::PointTemperatureOutOfRange {
                table: Battery,
                index: 0,
                value: -301,
            }],
        ),
        case(
            "point above 1100",
            |c| c.battery_table.points[3].temperature_deci_c = 1101,
            vec![Violation::PointTemperatureOutOfRange {
                table: Battery,
                index: 3,
                value: 1101,
            }],
        ),
        case(
            "points at the bounds",
            |c| {
                c.battery_table.points[0].temperature_deci_c = -300;
                c.battery_table.points[3].temperature_deci_c = 1100;
            },
            vec![],
        ),
    ]);
}

#[test]
fn table_endpoints_follow_the_pi_reading() {
    use Table::{Air, Battery};
    run(vec![
        case(
            "first point above off",
            |c| c.battery_table.points[0].temperature_deci_c = 271,
            vec![Violation::FirstPoint { table: Battery }],
        ),
        case(
            "first point below off is fine",
            |c| c.battery_table.points[0].temperature_deci_c = 200,
            vec![],
        ),
        case(
            "first point not 0 %",
            |c| c.air_table.points[0].duty_pct = 1,
            vec![Violation::FirstPoint { table: Air }],
        ),
        case(
            "last point 99 %",
            |c| c.battery_table.points[3].duty_pct = 99,
            vec![Violation::LastPointNot100 { table: Battery }],
        ),
        case(
            "point at on below fan_min",
            |c| c.battery_table.points[1].duty_pct = 29,
            vec![Violation::BelowFanMin {
                table: Battery,
                index: 1,
            }],
        ),
        case(
            "fan_min above both on points",
            |c| c.fan_min_pct = 31,
            vec![
                Violation::BelowFanMin {
                    table: Battery,
                    index: 1,
                },
                Violation::BelowFanMin {
                    table: Air,
                    index: 1,
                },
            ],
        ),
        case(
            "point between off and on may be below fan_min",
            |c| {
                c.battery_table = table(&[
                    point(270, 0),
                    point(285, 10),
                    point(300, 30),
                    point(450, 100),
                ]);
            },
            vec![],
        ),
    ]);
}

#[test]
fn thresholds_keep_their_order() {
    fn order(lower: &'static str, upper: &'static str) -> Vec<Violation> {
        vec![Violation::Order { lower, upper }]
    }
    run(vec![
        case(
            "ov recover at trip",
            |c| c.ov_recover_mv = c.ov_trip_mv,
            order("ov_recover_mV", "ov_trip_mV"),
        ),
        case(
            "uv recover at trip",
            |c| c.uv_recover_mv = c.uv_trip_mv,
            order("uv_trip_mV", "uv_recover_mV"),
        ),
        case(
            "uv recover at ov recover",
            |c| c.uv_recover_mv = c.ov_recover_mv,
            order("uv_recover_mV", "ov_recover_mV"),
        ),
        case(
            "charge ut",
            |c| c.chg_ut_recover_deci_c = c.chg_ut_trip_deci_c,
            order("chg_ut_trip_dC", "chg_ut_recover_dC"),
        ),
        case(
            "charge ot",
            |c| c.chg_ot_recover_deci_c = c.chg_ot_trip_deci_c,
            order("chg_ot_recover_dC", "chg_ot_trip_dC"),
        ),
        case(
            "discharge ut",
            |c| c.dis_ut_recover_deci_c = c.dis_ut_trip_deci_c,
            order("dis_ut_trip_dC", "dis_ut_recover_dC"),
        ),
        case(
            "discharge ot",
            |c| c.dis_ot_recover_deci_c = c.dis_ot_trip_deci_c,
            order("dis_ot_recover_dC", "dis_ot_trip_dC"),
        ),
        case(
            "air",
            |c| c.air_recover_deci_c = c.air_ot_deci_c,
            order("air_recover_dC", "air_ot_dC"),
        ),
        case(
            "battery fan hysteresis",
            |c| c.battery_fan_off_deci_c = c.battery_fan_on_deci_c,
            order("battery_fan_off_dC", "battery_fan_on_dC"),
        ),
        case(
            "air fan hysteresis",
            |c| c.air_fan_off_deci_c = c.air_fan_on_deci_c,
            order("air_fan_off_dC", "air_fan_on_dC"),
        ),
        case(
            "one step apart is fine",
            |c| {
                c.ov_recover_mv = c.ov_trip_mv - 1;
                c.chg_ot_recover_deci_c = c.chg_ot_trip_deci_c - 1;
            },
            vec![],
        ),
    ]);
}

#[test]
fn soc_window_is_min_below_max_up_to_100_percent() {
    run(vec![
        case(
            "min equals max",
            |c| c.soc_min_centi_pct = c.soc_max_centi_pct,
            vec![Violation::SocRange {
                min: 8500,
                max: 8500,
            }],
        ),
        case(
            "min above max",
            |c| c.soc_min_centi_pct = 9000,
            vec![Violation::SocRange {
                min: 9000,
                max: 8500,
            }],
        ),
        case(
            "max above 100 %",
            |c| c.soc_max_centi_pct = 10_001,
            vec![Violation::SocRange {
                min: 2000,
                max: 10_001,
            }],
        ),
        case(
            "0 % to 100 % is fine",
            |c| {
                c.soc_min_centi_pct = 0;
                c.soc_max_centi_pct = 10_000;
            },
            vec![],
        ),
    ]);
}

#[test]
fn every_period_must_be_nonzero() {
    run(vec![
        case(
            "voltage_detect_ms",
            |c| c.voltage_detect_ms = 0,
            vec![Violation::ZeroPeriod("voltage_detect_ms")],
        ),
        case(
            "temp_detect_ms",
            |c| c.temp_detect_ms = 0,
            vec![Violation::ZeroPeriod("temp_detect_ms")],
        ),
        case(
            "recover_ms",
            |c| c.recover_ms = 0,
            vec![Violation::ZeroPeriod("recover_ms")],
        ),
        case(
            "overload_ms",
            |c| c.overload_ms = 0,
            vec![Violation::ZeroPeriod("overload_ms")],
        ),
        case(
            "fan_fault_ms",
            |c| c.fan_fault_ms = 0,
            vec![Violation::ZeroPeriod("fan_fault_ms")],
        ),
        case(
            "fan_offline_ms",
            |c| c.fan_offline_ms = 0,
            vec![Violation::ZeroPeriod("fan_offline_ms")],
        ),
        case(
            "telemetry_ms",
            |c| c.telemetry_ms = 0,
            vec![Violation::ZeroPeriod("telemetry_ms")],
        ),
        case(
            "heartbeat_ms",
            |c| c.heartbeat_ms = 0,
            vec![Violation::ZeroPeriod("heartbeat_ms")],
        ),
        case(
            "mcu_pi_timeout_ms",
            |c| c.mcu_pi_timeout_ms = 0,
            vec![Violation::ZeroPeriod("mcu_pi_timeout_ms")],
        ),
        case(
            "pi_data_timeout_ms",
            |c| c.pi_data_timeout_ms = 0,
            vec![Violation::ZeroPeriod("pi_data_timeout_ms")],
        ),
    ]);
}

#[test]
fn temperatures_stay_within_the_encoding_bounds() {
    fn out(field: &'static str, value: i16) -> Vec<Violation> {
        vec![Violation::TemperatureOutOfRange { field, value }]
    }
    run(vec![
        case(
            "discharge ut trip -301",
            |c| c.dis_ut_trip_deci_c = -301,
            out("dis_ut_trip_dC", -301),
        ),
        case(
            "terminal ot 1101",
            |c| c.terminal_ot_deci_c = 1101,
            out("terminal_ot_dC", 1101),
        ),
        case(
            "battery fan on 1101",
            |c| {
                // Keep the table consistent, so only the bound fails.
                c.battery_fan_on_deci_c = 1101;
                c.battery_table.points[1].temperature_deci_c = 1099;
                c.battery_table.points[2].temperature_deci_c = 1100;
                c.battery_table.points[3].temperature_deci_c = 1100;
                c.battery_table.count = 3;
                c.battery_table.points[3] = FanPoint::default();
                c.battery_table.points[2].duty_pct = 100;
            },
            out("battery_fan_on_dC", 1101),
        ),
        case(
            "at the bounds is fine",
            |c| {
                c.dis_ut_trip_deci_c = -300;
                c.terminal_ot_deci_c = 1100;
            },
            vec![],
        ),
        case(
            "terminal delta is a difference, not bounded",
            |c| c.terminal_delta_deci_c = 5000,
            vec![],
        ),
    ]);
}

#[test]
fn description_text_rules_are_not_checked() {
    // DD-67: only the §7 list rejects a CONFIG; these are kept and handled by
    // the caller (firmware-requests.md §6 asks firmware about them).
    run(vec![
        case(
            "profile 1",
            |c| c.current_profile = CurrentProfile::Upgrade,
            vec![],
        ),
        case(
            "profile 2",
            |c| c.current_profile = CurrentProfile::from(2),
            vec![],
        ),
        case(
            "offline action 3",
            |c| c.fan_offline_action = FanOfflineAction::from(3),
            vec![],
        ),
        case(
            "flag set without RPMs",
            |c| c.flags = ConfigFlags(ConfigFlags::BATTERY_RPM_CALIBRATED),
            vec![],
        ),
        case(
            "RPMs with flag clear",
            |c| c.air_table.points[2].expected_rpm = 1800,
            vec![],
        ),
        // The two §7 items without numbers in the draft.
        case("capacity 1 mAh", |c| c.capacity_mah = 1, vec![]),
        case("capacity u32::MAX", |c| c.capacity_mah = u32::MAX, vec![]),
        case("any current limits", |c| c.chg_oc_ma = u32::MAX, vec![]),
    ]);
}

#[test]
fn rpm_calibration_is_usable_only_with_the_flag_and_every_rpm() {
    let mut c = reference();
    assert!(!c.rpm_calibrated(Table::Battery));
    assert!(!c.rpm_calibrated(Table::Air));

    // Flag set, but the RPMs are still 0: E12 stays disabled.
    c.flags = ConfigFlags(ConfigFlags::BATTERY_RPM_CALIBRATED | ConfigFlags::AIR_RPM_CALIBRATED);
    assert!(c.flags.rpm_calibrated(Table::Battery));
    assert!(!c.rpm_calibrated(Table::Battery));

    // Every nonzero-duty point calibrated; the 0 % point may stay 0 RPM.
    for (i, rpm) in [(1, 900), (2, 1900), (3, 2900)] {
        c.battery_table.points[i].expected_rpm = rpm;
    }
    assert!(c.rpm_calibrated(Table::Battery));
    assert!(!c.rpm_calibrated(Table::Air));

    // The flag decides, not the RPMs alone.
    c.flags = ConfigFlags(ConfigFlags::AIR_RPM_CALIBRATED);
    assert!(!c.rpm_calibrated(Table::Battery));
    assert_eq!(c.violations(), vec![]);
}

#[test]
fn rejection_messages_name_the_problem() {
    let schema = Rejection::UnsupportedSchema(2).to_string();
    assert!(schema.contains("schema 2"), "{schema}");
    let identity = Rejection::IdentityMismatch {
        config: REFERENCE_IDENTITY,
        telemetry: Identity {
            boot_counter: 7,
            config_revision: 1,
        },
    }
    .to_string();
    assert!(identity.contains("boot 7"), "{identity}");
    assert_eq!(
        FirmwareVersion {
            major: 1,
            minor: 2,
            patch: 3
        }
        .to_string(),
        "1.2.3"
    );
}
