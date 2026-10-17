// Exercise the production cache/locking code without requiring a modem or a
// running D-Bus daemon. The full backend is separately cross-compiled for Linux.
// Requires cargo on PATH; Windows GNU hosts also need their usual linker tools.
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdir, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

const project = resolve(dirname(fileURLToPath(import.meta.url)), '..')

void test('production modem snapshots: concurrency, expiry, invalidation, readback and cancellation', {
  timeout: 120_000,
}, async () => {
  const location = resolve(project, 'backend/target/modem-snapshot-tests')
  await mkdir(resolve(location, 'src'), { recursive: true })
  await writeFile(resolve(location, 'Cargo.toml'), `[package]
name="modem-snapshot-tests"
version="0.1.0"
edition="2021"
[dependencies]
tokio={version="=1.48.0",features=["rt","macros","sync","time"]}
[workspace]
`)
  // Only the error carrier is stubbed: include! compiles serial.rs unchanged,
  // including its real tests. No property or AT calls are made by these tests.
  await writeFile(resolve(location, 'src/lib.rs'), `extern crate self as zbus;
#[derive(Debug)]
pub enum Error { Failure(String) }
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Failure(message) => f.write_str(message) }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[path = "${resolve(project, 'backend/src/serial.rs').replaceAll('\\', '/')}"]
mod serial;
`)
  const result = spawnSync(process.platform === 'win32' ? 'cargo.exe' : 'cargo', [
    'test', '--offline', '--manifest-path', resolve(location, 'Cargo.toml'), '--lib',
  ], { cwd: project, encoding: 'utf8', timeout: 110_000 })
  assert.ifError(result.error)
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`)
  assert.match(result.stdout, /snapshots_coalesce_expire_and_reject_obsolete_results .*ok/)
  assert.match(result.stdout, /confirmation_retries_after_failed_write_and_failed_probe .*ok/)
  assert.match(result.stdout, /confirmation_does_not_wait_for_late_write_reply_or_accept_unconfirmed_success .*ok/)
})
