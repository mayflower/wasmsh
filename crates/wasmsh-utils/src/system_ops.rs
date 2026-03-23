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

pub(crate) fn util_env(ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    print_all_exported(ctx);
    0
}

pub(crate) fn util_printenv(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    if argv.len() >= 2 {
        if let Some(state) = ctx.state {
            let name = argv[1];
            if let Some(value) = state.env.exported_vars().get(name) {
                ctx.output.stdout(value.as_bytes());
                ctx.output.stdout(b"\n");
                return 0;
            }
        }
        return 1;
    }
    print_all_exported(ctx);
    0
}

pub(crate) fn util_id(ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    ctx.output
        .stdout(b"uid=1000(user) gid=1000(user) groups=1000(user)\n");
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
    }
    0
}

pub(crate) fn util_hostname(ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    ctx.output.stdout(b"wasmsh\n");
    0
}

pub(crate) fn util_sleep(_ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    // In wasmsh, sleep is a cooperative yield. For now, just return.
    0
}

pub(crate) fn util_date(ctx: &mut UtilContext<'_>, _argv: &[&str]) -> i32 {
    if let Some(state) = ctx.state {
        if let Some(d) = state.get_var("WASMSH_DATE") {
            ctx.output.stdout(d.as_bytes());
            ctx.output.stdout(b"\n");
            return 0;
        }
    }
    // Virtual date — return a fixed deterministic date
    ctx.output.stdout(b"2026-01-01 00:00:00 UTC\n");
    0
}
