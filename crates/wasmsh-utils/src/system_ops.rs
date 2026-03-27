//! System/env utilities: env, printenv, id, whoami, uname, hostname, sleep, date.

use crate::UtilContext;

pub(crate) fn print_all_exported(ctx: &mut UtilContext<'_>) {
    if let Some(state) = ctx.state {
        for (name, value) in &state.env.exported_vars() {
            let line = format!("{name}={value}\n");
            ctx.output.stdout(line.as_bytes());
        }
    }
}

pub(crate) fn util_env(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut ignore_env = false;
    let mut unset_vars: Vec<&str> = Vec::new();
    let mut null_sep = false;
    let mut extra_vars: Vec<(&str, &str)> = Vec::new();
    let mut i = 1;

    while i < argv.len() {
        let arg = argv[i];
        if arg == "-i" || arg == "--ignore-environment" {
            ignore_env = true;
            i += 1;
        } else if arg == "-u" && i + 1 < argv.len() {
            unset_vars.push(argv[i + 1]);
            i += 2;
        } else if arg == "-0" || arg == "--null" {
            null_sep = true;
            i += 1;
        } else if let Some((k, v)) = arg.split_once('=') {
            extra_vars.push((k, v));
            i += 1;
        } else {
            break;
        }
    }

    let sep = if null_sep { "\0" } else { "\n" };

    if ignore_env {
        // Only print extra vars
        for (k, v) in &extra_vars {
            let line = format!("{k}={v}{sep}");
            ctx.output.stdout(line.as_bytes());
        }
    } else {
        if let Some(state) = ctx.state {
            for (name, value) in &state.env.exported_vars() {
                if unset_vars.contains(&name.as_str()) {
                    continue;
                }
                let line = format!("{name}={value}{sep}");
                ctx.output.stdout(line.as_bytes());
            }
        }
        for (k, v) in &extra_vars {
            let line = format!("{k}={v}{sep}");
            ctx.output.stdout(line.as_bytes());
        }
    }
    0
}

pub(crate) fn util_printenv(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut null_sep = false;
    let mut names = Vec::new();
    for arg in &argv[1..] {
        if *arg == "-0" || *arg == "--null" {
            null_sep = true;
        } else {
            names.push(*arg);
        }
    }
    if !names.is_empty() {
        if let Some(state) = ctx.state {
            let vars = state.env.exported_vars();
            for name in &names {
                if let Some(value) = vars.get(*name) {
                    ctx.output.stdout(value.as_bytes());
                    ctx.output.stdout(if null_sep { b"\0" } else { b"\n" });
                    return 0;
                }
            }
        }
        return 1;
    }
    print_all_exported(ctx);
    0
}

pub(crate) fn util_id(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut show_user = false;
    let mut show_group = false;
    let mut show_groups = false;
    let mut show_name = false;

    for arg in &argv[1..] {
        if arg.starts_with('-') && arg.len() > 1 {
            for ch in arg[1..].chars() {
                match ch {
                    'u' => show_user = true,
                    'g' => show_group = true,
                    'G' => show_groups = true,
                    'n' => show_name = true,
                    'r' => {} // real id, same as effective in VFS
                    _ => {}
                }
            }
        }
    }

    if show_user {
        if show_name {
            ctx.output.stdout(b"user\n");
        } else {
            ctx.output.stdout(b"1000\n");
        }
    } else if show_group {
        if show_name {
            ctx.output.stdout(b"user\n");
        } else {
            ctx.output.stdout(b"1000\n");
        }
    } else if show_groups {
        if show_name {
            ctx.output.stdout(b"user\n");
        } else {
            ctx.output.stdout(b"1000\n");
        }
    } else {
        ctx.output
            .stdout(b"uid=1000(user) gid=1000(user) groups=1000(user)\n");
    }
    0
}

pub(crate) fn util_whoami(ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    ctx.output.stdout(b"user\n");
    0
}

pub(crate) fn util_uname(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let args = &argv[1..];
    if args.is_empty() || args.contains(&"-s") {
        ctx.output.stdout(b"wasmsh\n");
    } else if args.contains(&"-a") {
        ctx.output.stdout(b"wasmsh wasmsh 0.1.0 wasm32 wasmsh\n");
    } else if args.contains(&"-m") {
        ctx.output.stdout(b"wasm32\n");
    } else if args.contains(&"-r") {
        ctx.output.stdout(b"0.1.0\n");
    } else if args.contains(&"-n") {
        ctx.output.stdout(b"wasmsh\n");
    } else if args.contains(&"-o") {
        ctx.output.stdout(b"wasmsh\n");
    } else if args.contains(&"-p") {
        ctx.output.stdout(b"wasm32\n");
    } else if args.contains(&"-v") {
        ctx.output.stdout(b"0.1.0\n");
    }
    0
}

pub(crate) fn util_hostname(ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    ctx.output.stdout(b"wasmsh\n");
    0
}

pub(crate) fn util_sleep(_ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    0
}

struct DateParts {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

fn parse_date_string(s: &str) -> DateParts {
    // Parse "YYYY-MM-DD HH:MM:SS" format
    let mut parts = DateParts {
        year: 2026,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };
    let s = s.trim();
    if let Some((date_part, rest)) = s.split_once(' ') {
        let dp: Vec<&str> = date_part.split('-').collect();
        if dp.len() == 3 {
            parts.year = dp[0].parse().unwrap_or(2026);
            parts.month = dp[1].parse().unwrap_or(1);
            parts.day = dp[2].parse().unwrap_or(1);
        }
        let time_part = rest.split_whitespace().next().unwrap_or("00:00:00");
        let tp: Vec<&str> = time_part.split(':').collect();
        if !tp.is_empty() {
            parts.hour = tp[0].parse().unwrap_or(0);
        }
        if tp.len() > 1 {
            parts.minute = tp[1].parse().unwrap_or(0);
        }
        if tp.len() > 2 {
            parts.second = tp[2].parse().unwrap_or(0);
        }
    } else {
        let dp: Vec<&str> = s.split('-').collect();
        if dp.len() == 3 {
            parts.year = dp[0].parse().unwrap_or(2026);
            parts.month = dp[1].parse().unwrap_or(1);
            parts.day = dp[2].parse().unwrap_or(1);
        }
    }
    parts
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const WEEKDAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

const WEEKDAY_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn day_of_week(y: u16, m: u8, d: u8) -> usize {
    // Zeller's formula (0=Sunday)
    let y = y as i32;
    let m = m as i32;
    let d = d as i32;
    let (y, m) = if m < 3 { (y - 1, m + 12) } else { (y, m) };
    let dow = (d + (13 * (m + 1)) / 5 + y + y / 4 - y / 100 + y / 400) % 7;
    // Zeller gives 0=Saturday, convert to 0=Sunday
    ((dow + 6) % 7) as usize
}

fn format_date(fmt: &str, parts: &DateParts) -> String {
    let mut result = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.next() {
                Some('Y') => result.push_str(&format!("{:04}", parts.year)),
                Some('m') => result.push_str(&format!("{:02}", parts.month)),
                Some('d') => result.push_str(&format!("{:02}", parts.day)),
                Some('H') => result.push_str(&format!("{:02}", parts.hour)),
                Some('M') => result.push_str(&format!("{:02}", parts.minute)),
                Some('S') => result.push_str(&format!("{:02}", parts.second)),
                Some('s') => result.push('0'), // epoch seconds, fake
                Some('F') => result.push_str(&format!(
                    "{:04}-{:02}-{:02}",
                    parts.year, parts.month, parts.day
                )),
                Some('T') => result.push_str(&format!(
                    "{:02}:{:02}:{:02}",
                    parts.hour, parts.minute, parts.second
                )),
                Some('A') => {
                    let dow = day_of_week(parts.year, parts.month, parts.day);
                    result.push_str(WEEKDAY_NAMES[dow]);
                }
                Some('a') => {
                    let dow = day_of_week(parts.year, parts.month, parts.day);
                    result.push_str(WEEKDAY_ABBR[dow]);
                }
                Some('B') => {
                    if parts.month >= 1 && parts.month <= 12 {
                        result.push_str(MONTH_NAMES[(parts.month - 1) as usize]);
                    }
                }
                Some('b') | Some('h') => {
                    if parts.month >= 1 && parts.month <= 12 {
                        result.push_str(MONTH_ABBR[(parts.month - 1) as usize]);
                    }
                }
                Some('Z') => result.push_str("UTC"),
                Some('z') => result.push_str("+0000"),
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('%') => result.push('%'),
                Some('e') => result.push_str(&format!("{:>2}", parts.day)),
                Some('I') => {
                    let h12 = if parts.hour == 0 {
                        12
                    } else if parts.hour > 12 {
                        parts.hour - 12
                    } else {
                        parts.hour
                    };
                    result.push_str(&format!("{h12:02}"));
                }
                Some('p') => {
                    result.push_str(if parts.hour < 12 { "AM" } else { "PM" });
                }
                Some('R') => {
                    result.push_str(&format!("{:02}:{:02}", parts.hour, parts.minute));
                }
                Some(c) => {
                    result.push('%');
                    result.push(c);
                }
                None => result.push('%'),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

pub(crate) fn util_date(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let base_str = ctx
        .state
        .and_then(|s| s.get_var("WASMSH_DATE"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "2026-01-01 00:00:00 UTC".to_string());

    let parts = parse_date_string(&base_str);

    // Find +FORMAT argument
    let mut format_arg: Option<&str> = None;
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i];
        if let Some(fmt) = arg.strip_prefix('+') {
            format_arg = Some(fmt);
            i += 1;
        } else if arg == "-d" || arg == "--date" {
            i += 2; // skip arg, ignore
        } else if arg == "-u" || arg == "-R" || arg == "-I" {
            i += 1; // accept, no special handling
        } else {
            i += 1;
        }
    }

    if let Some(fmt) = format_arg {
        let out = format_date(fmt, &parts);
        ctx.output.stdout(out.as_bytes());
        ctx.output.stdout(b"\n");
    } else {
        ctx.output.stdout(base_str.as_bytes());
        if !base_str.ends_with('\n') {
            ctx.output.stdout(b"\n");
        }
    }
    0
}
