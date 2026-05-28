# ztop v0.7 — Codebase Review Report

Date: 2026-05-18

---

## Critical (blocking)

### 1. Build fails — `Sparkline` not imported (`ui.rs:13`)

`Sparkline::default()` is used but not in the `use ratatui::widgets::{...}` list. Binary does not compile.
Also 5 unused imports left over: `Axis`, `Chart`, `Dataset`, `GraphType`, `symbols`.

**Fix:** add `Sparkline` to the import, remove unused ones.

---

## High Priority

### 2. Shell injection via log path (`collectors.rs:271`, `source.rs:447`)

```rust
// collectors.rs
let cmd = format!("tail -n {lines} -- {path} 2>/dev/null");
// source.rs
let remote_cmd = format!("tail -n {} -F -- {} 2>/dev/null", initial_lines, log_path);
```

Both strings are passed as a single SSH argument. `sshd` invokes the login shell with `-c <string>` — a `log_path` containing `;`, `$()`, or backticks executes arbitrary commands on every monitored server. The `--` stops `tail` from interpreting flags, but the shell has already tokenized the path before `tail` sees it.

The comment acknowledges this: `"// shell-escape пути — на MVP полагаемся..."` — must be fixed.

**Fix:** add a `shell_quote` helper and use it in both format strings:

```rust
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
```

### 3. `CollectorMsg::Reset` silently written as fake `LogStreamStatus` (`record.rs:133`)

```rust
CollectorMsg::Reset => Self::LogStreamStatus(LogStreamStatus::default()),
```

This writes `{ connected: false, reconnects: 0 }` to the JSONL file — a spurious "stream disconnected" event. During re-replay of that file, `apply_msg` interprets it as a real disconnect, corrupting the replay.

**Fix:** guard the recorder call in `main.rs` before `rec.write()`:

```rust
if !matches!(host_msg.msg, CollectorMsg::Reset) {
    rec.write(&host_msg);
}
```

### 4. DRY: `spawn_procs` / `spawn_sys` / `spawn_stats` are identical loops (`source.rs:229-390`)

~75 lines of identical `loop { fetch → send → next_interval → select! }`. A generic `spawn_periodic<F>` helper would collapse them to 3 single-line calls, making adding new periodic sources trivial.

---

## Medium Priority

### 5. Dead code `_link_helper` in `diagnose.rs:242`

`ZbxRoleAgg` is only used in `#[cfg(test)]`. The import at the top level is unused in non-test code; the dead helper function exists only to suppress the lint.

**Fix:** move the import inside `mod tests`, delete `_link_helper`.

### 6. `format!("{line}\n")` allocation per SSH poll (`collectors.rs:207-211`)

```rust
"load" => load_buf.push_str(&format!("{line}\n")),
```

Allocates a temporary `String` on every line per tick.

**Fix:** `push_str(line); push('\n');`

### 7. `.map(|v| v.clone())` → `.cloned()` (`record.rs:104-125`, 4 sites)

Non-idiomatic; replace with `.cloned()`.

### 8. ControlPath in `/tmp` with predictable name (`ssh.rs:47`)

`/tmp/ztop-ssh-%r@%h:%p` — on multi-user systems another local user can pre-create this path as a symlink. OpenSSH checks socket ownership so exploitation is non-trivial, but the pattern is a best-practice violation.

**Fix:** use `$XDG_RUNTIME_DIR` (mode 0700) or `~/.ssh/ztop-ctl-%r@%h:%p`.

### 9. `pct_decode` uses `from_utf8_lossy` (`dbcreds.rs:172`)

Silently corrupts non-ASCII passwords encoded as multi-byte percent sequences (e.g. `%C3%A9` → U+FFFD).

**Fix:** replace with `String::from_utf8(out).ok()?` to fail cleanly instead of mangling.

### 10. `CollectorMsg::Reset` leaks replay concern into general message type (`source.rs:106-122`)

`kind()` returns a fake `SourceKind::Procs` with an apologetic comment. The replay subsystem bleeds into the general data transport layer.

**Options:**
- Minimal: document the enum as replay-aware with a clear comment.
- Ideal: extract `Reset` into a separate `ReplayMsg` wrapper in `record.rs`.

---

## Low Priority

### 11. Missing parentheses in `classify()` (`collectors.rs:255-258`)

```rust
|| l.contains("housekeeper") && l.contains("delete")
```

Logic is correct (Rust `&&` > `||` precedence), but reads ambiguously.

**Fix:** add explicit parentheses: `|| (l.contains("housekeeper") && l.contains("delete"))`

### 12. Unused `tokio` feature `signal` (`Cargo.toml`)

No `tokio::signal` usage found anywhere. Ctrl+C is handled via crossterm. Remove `signal` from the feature list.

### 13. `diagnose()` called ~3N times per render frame (`ui.rs:21`, `454`, `551-562`)

Once in the header, once in `draw_overview_table`, twice in `draw_overview_aggregate`. Compute once at top of `draw()` and pass down. Only matters at 20+ hosts.

### 14. `fetch_log_tail` may be dead code (`collectors.rs:269`)

Likely superseded by streaming `spawn_log_stream`. Confirm and remove if unused.

---

## Design Questions

- `rule_db_lock_contention` in `diagnose.rs` uses only one source (`"db"`), while the module header states "rules require ≥2 sources." Intentional exception?
- `App` derives `Clone` but cloning shares `Arc<AtomicU32>` / `Arc<AtomicBool>` replay state. `App` is never cloned today, but `derive(Clone)` is a trap for future contributors — add a doc comment.

---

## Positives

- Clean layered architecture (SSH / zabbix.stats / DB as independent sources with independent liveness)
- Excellent test coverage for all pure parsers (`collectors`, `db`, `hosts`, `zbxstats`, `dbcreds`, `record`, `app`)
- Correct exponential backoff with cap and reset-on-success; unit-tested
- `kill_on_drop(true)` on all SSH subprocesses — no zombie leaks
- `PgConnection::Drop` aborts the background connection task — no task leak
- MySQL try-or-default per-query pattern handles schema variance across MySQL 5.7 / 8.0 / MariaDB cleanly
- `conflicts_with = "record"` on `--replay` arg prevents invalid CLI combinations at parse time
- `[profile.release]` with LTO thin + strip is production-appropriate

---

## Summary

| Severity | Count |
|----------|-------|
| Critical (build-blocking) | 1 |
| High (security / correctness) | 3 |
| Medium (quality / safety) | 6 |
| Low (polish) | 4 |
| Design questions | 2 |
