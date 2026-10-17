import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import process from 'node:process'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { dashboardTimingShell } from './dashboard-diagnostics.ts'

const project = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const bash = process.platform === 'win32' ? 'C:/Program Files/Git/bin/bash.exe' : '/bin/bash'

const mockDbus = String.raw`#!/bin/sh
case "$*" in
    *ConnectionManager.GetContexts*) cat <<'EOF'
object path "/ril_0/context1"
string "Type"
variant string "internet"
string "AccessPointName"
variant string ""
object path "/ril_0/context3"
string "Type"
variant string "internet"
string "AccessPointName"
variant string "configured-apn"
EOF
    ;;
    *NetworkRegistration.GetProperties*)
        printf 'string "Status"\nvariant string "%s"\nstring "MobileCountryCode"\nvariant string "454"\n' "$FIXTURE_REG";;
    *NetworkMonitor.GetServingCellInformation*)
        printf 'string "Technology"\nvariant string "%s"\n' "$FIXTURE_TECH";;
    *org.ofono.Modem.GetProperties*)
        printf 'string "Powered"\nvariant boolean true\nstring "Online"\nvariant boolean true\n';;
    *) echo 'method return';;
esac
`

const mockTime = String.raw`#!/bin/sh
[ "$1" = time ] || exit 127
shift
[ "$1" = -p ] && shift
"$@"
code=$?
printf 'real 0.012\nuser 0.000\nsys 0.000\n' >&2
exit "$code"
`

void test('generated BusyBox script selects context/RAT and records timing and deferred responses', {
  skip: !existsSync(bash), timeout: 60_000,
}, async () => {
  for (const [registration, technology] of [['registered', 'lte'], ['roaming', 'nr'], ['searching', 'lte']]) {
    const location = await mkdtemp(resolve(project, 'backend/target/dashboard-timing-test-'))
    const bin = resolve(location, 'bin')
    await mkdir(bin)
    const scripts = {
      'dbus-send': mockDbus,
      busybox: mockTime,
      ping: '#!/bin/sh\nprintf "Mock ping %s\\n" "$*"\n',
      ping6: '#!/bin/sh\nprintf "Mock ping6 %s\\n" "$*"\n',
      wget: '#!/bin/sh\ncase "$*" in */api/qos*) echo \'{"status":"deferred"}\';; *) echo \'{"status":"ok"}\';; esac\n',
    }
    for (const [name, source] of Object.entries(scripts)) await writeFile(resolve(bin, name), source, { mode: 0o755 })
    await writeFile(resolve(location, 'dashboard-timing.sh'), dashboardTimingShell)
    const launch = String.raw`
        if command -v cygpath >/dev/null 2>&1; then location=$(cygpath -u "$1"); else location=$1; fi
        export PATH="$location/bin:$PATH"
        export OUT_DIR="$location/results" MODE=both ROUNDS=1
        export FIXTURE_REG="$2" FIXTURE_TECH="$3"
        sh "$location/dashboard-timing.sh"
    `
    const result = spawnSync(bash, ['--noprofile', '--norc', '-c', launch, '--', location, registration!, technology!], {
      encoding: 'utf8', timeout: 20_000,
    })
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`)
    const commands = await readFile(resolve(location, 'results/commands.log'), 'utf8')
    assert.match(commands, /\/ril_0\/context3 org.ofono.ConnectionContext.GetProperties/)
    assert.match(commands, /ping -c 1 -W 2 1\.1\.1\.1/)
    assert.doesNotMatch(commands, /SetProperty|SimRefresh|SPASENGMD/)
    if (registration === 'searching') {
      assert.doesNotMatch(commands, /SendAtcmd|IpMultimediaSystem/)
      assert.match(commands, /SKIP diagnostics/)
    } else {
      for (const command of technology === 'nr'
        ? ['AT+SPENGMD=0,14,1', 'AT+SPENGMD=0,14,2']
        : ['AT+SPENGMD=0,6,0', 'AT+SPENGMD=0,6,6']) assert.ok(commands.includes(command))
      assert.ok(commands.includes('AT+CGEQOSRDP'))
    }
    const samples = await readFile(resolve(location, 'results/samples.tsv'), 'utf8')
    assert.match(samples, /api_qos\t0\t0.012\tdeferred/)
    const summary = await readFile(resolve(location, 'results/summary.tsv'), 'utf8')
    assert.match(summary, /raw_context\t1\t0.012\t0.012\t0.012/)
    assert.match(summary, /api_qos\t1\t0.012\t0.012\t0.012\t0\t1/)
  }
})
