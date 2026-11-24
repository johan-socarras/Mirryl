param(
    [int]$IntervalSeconds = 25,
    # Where the log and targets.txt live. Beside this script by default, which
    # is what the installer's scheduled task uses. The Microsoft Store package
    # passes %LOCALAPPDATA%\Mirryl instead: its own folder is read-only
    # (main.rs, start_store_watcher).
    [string]$DataDir = $PSScriptRoot,
    # The adb to call. Empty means the one beside this script. The Store
    # package passes its app execution alias (mirryl-adb.exe): Windows only
    # lets a process running INSIDE the package execute the package's adb.exe,
    # and this PowerShell is a Windows program, so Windows starts it outside,
    # where every call fails with "Access is denied". The alias starts that
    # same adb.exe inside the package whoever calls it.
    [string]$AdbPath = ""
)

$ScriptDir = $PSScriptRoot

# The scheduled task starts this at boot with no one logged on (S4U), and
# Windows can start it with APPDATA empty (and possibly the other per-user
# variables): every cycle would then die on Join-Path $env:APPDATA with
# "Cannot bind argument to parameter 'Path' because it is null", and nothing
# would reconnect. Fill in whatever is missing, in this process's environment
# so the adb it starts inherits them too (adb finds its key under the user
# profile). Order: the user profile, then the folder this script lives in
# (%LOCALAPPDATA%\Mirryl for the regular install), and only last what Windows
# answers for the known folder. In that kind of start Windows can answer
# C:\Users\Default\AppData\... - the template profile, not this user's - so an
# answer under \Users\Default is never taken: the watcher would read a phone
# list that does not exist and treat every phone as a stranger.
$script:EnvFilled = @()
function Set-MissingEnv([string]$Name, [scriptblock[]]$Candidates) {
    $current = [Environment]::GetEnvironmentVariable($Name)
    if ($current -and $current -notmatch '\\Users\\Default(\\|$)') { return }
    foreach ($c in $Candidates) {
        $v = try { & $c } catch { $null }
        if ($v -and $v -notmatch '\\Users\\Default(\\|$)') {
            [Environment]::SetEnvironmentVariable($Name, $v)
            $script:EnvFilled += "$Name=$v"
            return
        }
    }
}
$localGuess = if ((Split-Path $PSScriptRoot -Leaf) -eq 'Mirryl') { Split-Path $PSScriptRoot -Parent } else { $null }
Set-MissingEnv 'USERPROFILE' @(
    { if ($localGuess -and (Split-Path $localGuess -Leaf) -eq 'Local') { Split-Path (Split-Path $localGuess -Parent) -Parent } },
    { [Environment]::GetFolderPath('UserProfile') })
Set-MissingEnv 'LOCALAPPDATA' @(
    { if ($env:USERPROFILE) { Join-Path $env:USERPROFILE 'AppData\Local' } },
    { $localGuess },
    { [Environment]::GetFolderPath('LocalApplicationData') })
Set-MissingEnv 'APPDATA' @(
    { if ($env:USERPROFILE) { Join-Path $env:USERPROFILE 'AppData\Roaming' } },
    { [Environment]::GetFolderPath('ApplicationData') })
Set-MissingEnv 'TEMP' @(
    { if ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA 'Temp' } },
    { [IO.Path]::GetTempPath() })
Set-MissingEnv 'TMP' @({ $env:TEMP })

$LogFile = Join-Path $DataDir "watcher.log"
$TargetsFile = Join-Path $DataDir "targets.txt"

# Use the adb that ships beside the app, never whatever happens to be on PATH.
# A different adb build kills the running server and starts its own ("adb
# server version doesn't match"), dropping every connection the app holds -
# once every 25 s, forever. Falls back to PATH only if the bundled copy is gone.
$Adb = Join-Path $ScriptDir "platform-tools\adb.exe"
if ($AdbPath) { $Adb = $AdbPath }
elseif (-not (Test-Path -LiteralPath $Adb)) { $Adb = "adb" }

function Write-Log {
    param([string]$Message)
    $line = "[{0}] {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $Message
    Add-Content -Path $LogFile -Value $line
    if ((Test-Path $LogFile) -and (Get-Item $LogFile).Length -gt 2MB) {
        $tail = Get-Content $LogFile -Tail 500
        Set-Content $LogFile $tail
    }
}

# Every transport adb currently lists, named the way adb names it (the USB
# serial, or `ip:port` for a Wi-Fi transport), with its state: `device`,
# `offline`, `unauthorized`... Returned bare (a Hashtable travels the pipeline
# as one object; do not wrap it in @()).
function Get-AdbTransportStates {
    $output = (& $Adb devices) -split "`r?`n"
    $states = @{}
    foreach ($line in $output) {
        # adb prints "<transport>\t<state>". Split on the TAB, not on
        # whitespace: a transport name can carry a space - the mDNS name adb
        # renames on a conflict, `adb-XXXX-yyyy (2)._adb-tls-connect._tcp` -
        # and the header line has no TAB.
        $parts = $line.TrimEnd() -split "`t"
        if ($parts.Count -ne 2) { continue }
        if (-not $parts[0] -or -not $parts[1]) { continue }
        $states[$parts[0]] = $parts[1]
    }
    return $states
}

# Only the transports in state `device`: the connections. Callers wrap the
# result in @(): PowerShell unrolls a one-element array on return.
function Get-ConnectedTransports {
    $states = Get-AdbTransportStates
    $found = @()
    foreach ($t in $states.Keys) {
        if ($states[$t] -eq "device") { $found += $t }
    }
    return $found
}

# MUST match SEAL_PEPPER in main.rs and WifiAdbHelper.SEAL_PEPPER (Android)
# exactly - all three compute the same hash independently. See main.rs's
# "Android seal" section for why this exists and its honest limit.
function Send-Seal {
    param([string]$Target)
    $pepper = "mirryl-seal-v1-8f2c"
    $ts = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes("${pepper}:${ts}")
    $hashHex = -join ($sha256.ComputeHash($bytes) | ForEach-Object { $_.ToString("x2") })
    $sha256.Dispose()
    $json = '{"v":"' + $hashHex + '","ts":' + $ts + '}'
    $tmp = Join-Path $env:TEMP "mirryl-seal-$ts.json"
    # ASCII, not the default utf8: Windows PowerShell 5.1's utf8 writes a BOM,
    # which would land before the "{" and break the Android side's JSON parse.
    # The content is all ASCII (hex digits + JSON punctuation) so this is safe.
    Set-Content -LiteralPath $tmp -Value $json -NoNewline -Encoding ascii
    $null = Invoke-AdbTimeout -Arguments @("-s", $Target, "push", $tmp, "/sdcard/Android/data/com.mirryl.app/files/.mstate") -TimeoutSeconds 20
    Remove-Item -LiteralPath $tmp -ErrorAction SilentlyContinue
    Set-BatteryExemption -Target $Target
}

# Puts the phone app on Android's battery whitelist, every time a seal goes
# out. Without it Android files the app under "rare" and defers its alarm and
# jobs, and Wireless debugging may not come back on its own after a Wi-Fi
# drop. The APK does not ask for it itself; the app's grant (main.rs) sets it
# too.
# Repeating it is a no-op while it is in place, and puts it back if it got
# lost - which is also why the phone app's card says so. Same timeout as the
# seal push: a phone that goes mute must not hang the loop.
function Set-BatteryExemption {
    param([string]$Target)
    $null = Invoke-AdbTimeout -Arguments @("-s", $Target, "shell", "dumpsys", "deviceidle", "whitelist", "+com.mirryl.app") -TimeoutSeconds 20
}

# The PIN protection of the phone app (version 27 on) needs its accessibility
# service on; whoever can reach the phone's Accessibility settings can turn it
# off, and then the protected apps open without asking. Every few minutes a
# phone that has protected apps is asked, and its service goes back on if it
# is off. The line written says "PIN protection was off": the app shows those
# lines to the person (apps.rs, PIN_ALERT_MARK). An older phone app answers
# nothing here and is left alone.
$PinService = "com.mirryl.app/com.mirryl.app.AppLockService"
function Test-PinProtection {
    param([string]$Serial, [string]$Target)
    $answer = Invoke-AdbTimeout -Arguments @("-s", $Target, "shell", "am", "broadcast", "-n", "com.mirryl.app/.LockConfigReceiver", "--es", "op", "get") -TimeoutSeconds 20
    if (-not $answer) { return }
    $m = [regex]::Match($answer, '(?m)data="(.*)"\s*$')
    if (-not $m.Success) { return }
    try { $state = $m.Groups[1].Value | ConvertFrom-Json } catch { return }
    if ($state.service -or -not $state.locked -or @($state.locked).Count -eq 0) { return }
    $current = Invoke-AdbTimeout -Arguments @("-s", $Target, "shell", "settings", "get", "secure", "enabled_accessibility_services") -TimeoutSeconds 20
    $current = if ($current) { $current.Trim() } else { "" }
    $list = if ((-not $current) -or ($current -eq "null")) { $PinService } else { "${current}:$PinService" }
    if ($list -match "'") { return }
    $null = Invoke-AdbTimeout -Arguments @("-s", $Target, "shell", "settings", "put", "secure", "enabled_accessibility_services", "'$list'") -TimeoutSeconds 20
    $null = Invoke-AdbTimeout -Arguments @("-s", $Target, "shell", "settings", "put", "secure", "accessibility_enabled", "1") -TimeoutSeconds 20
    Write-Log "PIN protection was off on $Serial (its service had been turned off on the phone) - turned it back on."
}

# The same reading of a yes/no value that `loose_bool` does on the app's side
# (main.rs). PowerShell's own truthiness says that ANY non-empty string is true,
# so a devices.json edited by hand with "cable_only": "false" would read as
# FALSE for the app and TRUE here - the phone would be dialled by nobody and
# nobody would say why. Keeping the two readings identical is the whole point.
# Returns $true, $false, or the string 'unreadable' - the same three answers
# `loose_bool` gives, because it can return an error and PowerShell truthiness
# cannot. A disagreement here would mean the app refuses to read the file
# while the watcher happily keeps dialling a phone the person asked it to
# forget.
function ConvertTo-LooseBool {
    param($Value)
    if ($null -eq $Value) { return $false }
    if ($Value -is [bool]) { return $Value }
    if ($Value -is [int] -or $Value -is [long]) { return ($Value -ne 0) }
    if ($Value -is [double] -or $Value -is [decimal] -or $Value -is [single]) {
        # serde reads a whole number as an integer; anything with a fraction
        # (or past i64) is an error on the app's side, so it is one here too.
        $whole = [Math]::Truncate([double]$Value)
        if (([double]$Value -ne $whole) -or ([Math]::Abs([double]$Value) -gt 9.2233720368547758e18)) {
            return 'unreadable'
        }
        return ($whole -ne 0)
    }
    if ($Value -is [string]) {
        switch ($Value.Trim().ToLowerInvariant()) {
            'true'  { return $true }
            '1'     { return $true }
            'yes'   { return $true }
            'false' { return $false }
            '0'     { return $false }
            'no'    { return $false }
            ''      { return $false }
            default { return 'unreadable' }
        }
    }
    return 'unreadable'
}

# The phones the app has on file, read from the same devices.json main.rs
# writes (an array of rows; `serial` is absent on rows saved without one, and
# such rows are simply not reachable via mDNS from here - they get
# a serial stamped on their next pairing). Returns a serial -> host_port map,
# NOT just the serials: the serial is no secret (the phone multicasts it in the
# clear whenever wireless debugging is on), so a stranger who copies it into
# their own announcement would otherwise get this loop to dial THEIR ip:port
# every cycle. With the address on file the loop tries the saved one first and
# only falls back to the announced one - same order qr_pairing.rs already uses.
# Empty when the file is missing, empty or unreadable: then nothing found by
# mDNS is ours.
function Get-KnownSerials {
    $devicesFile = Join-Path $env:APPDATA "com.awadb.assistant\devices.json"
    # Ordinal comparer, not the @{} literal: @{} compares keys case-INSENSITIVELY,
    # and serials are compared case-sensitively everywhere else.
    # Returned bare: a Hashtable travels the pipeline as ONE object, so the
    # caller must not wrap it in @() (that would make .Count 1 when empty).
    $known = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
    # Serials the app marked "cable-only" with "Forget Wi-Fi address". They stay
    # in $known - they are ours, they get their seal over USB, and
    # calling them unknown would fill the log with "Ignoring unknown device" for
    # a phone on file - but the mDNS block must not dial them: forgetting the
    # address means "stop looking for it on the network", and a cycle that
    # dialled it anyway would put the phone back on Wi-Fi 25 s after the person
    # asked for the opposite.
    $script:CableOnlySerials = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
    # Mirrors the app's `registry_unreadable`: a devices.json the app refuses
    # to read must not be written by the watcher either, or the file the app is
    # protecting gets rewritten behind its back.
    $script:RegistryUnreadable = $false
    if ($null -eq $script:CableOnlyLogged) {
        $script:CableOnlyLogged = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
    }
    if (-not (Test-Path -LiteralPath $devicesFile)) { return $known }
    try {
        $raw = Get-Content -LiteralPath $devicesFile -Raw
        if (-not $raw -or -not $raw.Trim()) { return $known }
        $rows = ConvertFrom-Json $raw
        foreach ($row in @($rows)) {
            $s = $row.serial
            if (-not (($s -is [string]) -and $s.Trim())) { continue }
            # host_port is "" on rows added over USB and missing on old rows;
            # the serial still counts as known, it just has no saved address.
            $hp = $row.host_port
            if (($hp -is [string]) -and $hp.Trim()) { $hp = $hp.Trim() } else { $hp = $null }
            $known[$s.Trim()] = $hp
            if ($row.PSObject.Properties.Name -contains 'cable_only') {
                $flag = ConvertTo-LooseBool $row.cable_only
                if ($flag -is [string]) {
                    # The app cannot read this file at all, so its rows mean
                    # nothing: say so once and stop writing to it.
                    if (-not $script:RegistryUnreadable) {
                        Write-Log "devices.json has a cable_only value the app cannot read; not writing to it until it is fixed."
                    }
                    $script:RegistryUnreadable = $true
                } elseif ($flag) {
                    $script:CableOnlySerials[$s.Trim()] = $true
                }
            }
        }
    } catch {
        Write-Log "Could not read devices.json: $($_.Exception.Message)"
        $script:RegistryUnreadable = $true
    }
    # A serial that stopped being cable-only (re-paired) may be logged again.
    # A key is either a bare serial (the mDNS path) or "<target>|<serial>" (the
    # targets.txt one): purge by the serial part, or the composite key is wiped
    # every cycle and its notice repeats for ever.
    foreach ($seen in @($script:CableOnlyLogged.Keys)) {
        $serialPart = if ($seen.Contains('|')) { $seen.Substring($seen.IndexOf('|') + 1) } else { $seen }
        if (-not $script:CableOnlySerials.ContainsKey($serialPart)) { $script:CableOnlyLogged.Remove($seen) }
    }
    return $known
}

# Writes the address a phone on file was just reached at into devices.json,
# so the next cycle - and the app - dial THAT one first instead of a stale
# one: once the mDNS announcement goes quiet, a dead address on file would
# leave the phone off until someone typed `adb connect` by hand. Only the
# host_port of the row with this serial changes; the write is the same shape
# as write_devices in main.rs (temp file, then rename) and UTF-8 without a
# BOM, which is what serde_json on the app side reads - a BOM would make it
# read an EMPTY device list. Anything going wrong here is logged and ignored:
# the connection is already up, the address is a convenience.
function Save-KnownAddress {
    param([string]$Serial, [string]$HostPort)
    if (-not $Serial -or -not $HostPort -or $HostPort -notmatch ':') { return }
    if ($script:RegistryUnreadable) { return }
    $devicesFile = Join-Path $env:APPDATA "com.awadb.assistant\devices.json"
    if (-not (Test-Path -LiteralPath $devicesFile)) { return }
    try {
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        $raw = [System.IO.File]::ReadAllText($devicesFile, $utf8)
        if (-not $raw -or -not $raw.Trim()) { return }
        # Two steps, like Get-KnownSerials: @(ConvertFrom-Json ...) in PowerShell 5.1
        # wraps the whole list as ONE element; @($rows) on the variable does not.
        $rows = ConvertFrom-Json $raw
        $rows = @($rows)
        $changed = $false
        foreach ($row in $rows) {
            $rs = $row.serial
            if (-not (($rs -is [string]) -and ($rs.Trim() -ceq $Serial))) { continue }
            # "Forget Wi-Fi address" (cable_only in devices.json) says: this is
            # a cable phone, do not write an address into its row. The app
            # honours the same flag in its status poll; if only one of the two
            # did, the other would undo it within a cycle.
            if (($row.PSObject.Properties.Name -contains 'cable_only') -and
                ((ConvertTo-LooseBool $row.cable_only) -eq $true)) { continue }
            $current = $row.host_port
            if (($current -is [string]) -and ($current.Trim() -ceq $HostPort)) { continue }
            # Add-Member -Force: a row without host_port has no such property.
            $row | Add-Member -NotePropertyName host_port -NotePropertyValue $HostPort -Force
            $changed = $true
        }
        if (-not $changed) { return }
        # Read again and apply the change to THAT copy, not to the one read a
        # few milliseconds ago: everything else the app wrote in between - a
        # new alias, a new phone, a row removed - lives only in the fresh copy,
        # and serialising the stale one would erase it. Same shape as
        # reconcile_devices on the app's side, and it is also where "Forget
        # Wi-Fi address" is noticed if it landed inside that window.
        # Two steps, as above: @(ConvertFrom-Json ...) in PowerShell 5.1 wraps
        # the whole list as ONE element, and then nothing matches.
        $fresh = ConvertFrom-Json ([System.IO.File]::ReadAllText($devicesFile, $utf8))
        $fresh = @($fresh)
        $changed = $false
        foreach ($freshRow in $fresh) {
            if (($freshRow.serial -is [string]) -and ($freshRow.serial.Trim() -ceq $Serial) -and
                ($freshRow.PSObject.Properties.Name -contains 'cable_only') -and
                ((ConvertTo-LooseBool $freshRow.cable_only) -eq $true)) {
                Write-Log "  $Serial became cable-only while saving its address - not writing it."
                return
            }
            $rs2 = $freshRow.serial
            if (-not (($rs2 -is [string]) -and ($rs2.Trim() -ceq $Serial))) { continue }
            $freshPort = $freshRow.host_port
            if (($freshPort -is [string]) -and ($freshPort.Trim() -ceq $HostPort)) { continue }
            $freshRow | Add-Member -NotePropertyName host_port -NotePropertyValue $HostPort -Force
            $changed = $true
        }
        if (-not $changed) { return }
        # -InputObject, not the pipeline: piping a one-row array would unroll it
        # and write an object where the app expects a list.
        $json = ConvertTo-Json -InputObject $fresh -Depth 5
        $tmp = "$devicesFile.watcher.$PID.tmp"
        [System.IO.File]::WriteAllText($tmp, $json, $utf8)
        # MOVEFILE_REPLACE_EXISTING is the only swap Windows does in one step,
        # and it is the same one `std::fs::rename` uses on the app's side.
        # `Move-Item -Force` deletes the destination first, and a reader landing
        # in that gap sees devices.json missing - which the app reads as an
        # EMPTY device list. (`[System.IO.File]::Replace` is NOT the answer:
        # PowerShell turns the `$null` backup argument into "", so the call
        # throws every time, and even called correctly it is not one step.)
        $moved = $false
        $lastError = 0
        try {
            if (-not ('Mirryl.NativeMove' -as [type])) {
                Add-Type -Namespace Mirryl -Name NativeMove -MemberDefinition @'
[DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool MoveFileExW(string existing, string replaced, uint flags);
'@
            }
            # A reader with the file open denies the swap for as long as it
            # holds it (error 5). The app's poll holds it for microseconds, so
            # a few retries a few milliseconds apart are all it takes.
            for ($attempt = 0; $attempt -lt 10 -and -not $moved; $attempt++) {
                $moved = [Mirryl.NativeMove]::MoveFileExW($tmp, $devicesFile, 1)
                if (-not $moved) {
                    $lastError = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()
                    Start-Sleep -Milliseconds 15
                }
            }
            if (-not $moved) {
                Write-Log "  MoveFileEx refused the swap ten times (error $lastError); falling back."
            }
        } catch {
            Write-Log "  MoveFileEx unavailable ($($_.Exception.Message)); falling back."
        }
        if (-not $moved) {
            # -ErrorAction Stop, because without it a failed move is a
            # NON-terminating error: the catch below would never run, the temp
            # file would stay behind, and the log would say "saved" for an
            # address that was never written.
            Move-Item -LiteralPath $tmp -Destination $devicesFile -Force -ErrorAction Stop
        }
        Write-Log "  saved $HostPort as the address of $Serial."
    } catch {
        # Both the swap and its fallback can fail with the file locked by
        # something else (an editor, an indexer). Leaving the temp behind would
        # pile one up per failure.
        if ($tmp -and (Test-Path -LiteralPath $tmp)) {
            Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
        }
        Write-Log "  could not save the address of ${Serial}: $($_.Exception.Message)"
    }
}

# The serial an `_adb-tls-connect` instance name CLAIMS: `adb-<serial>-<token>`
# -> `<serial>`. Same rule as parse_adb_serial in qr_pairing.rs: drop the
# `adb-` prefix, cut at the LAST `-` (the token after it is a per-session
# random suffix; Android serials carry no `-`). A fully qualified name is
# trimmed to its first label first. $null for anything of another shape.
# Only a claim: whoever advertises chooses the name, and the serial is
# broadcast in the clear, so this filters what we DIAL, never who we trust.
function Get-MdnsSerial {
    param([string]$InstanceName)
    if (-not $InstanceName) { return $null }
    $label = ($InstanceName -split '\.')[0]
    if (-not $label.StartsWith("adb-")) { return $null }
    $rest = $label.Substring(4)
    $cut = $rest.LastIndexOf("-")
    if ($cut -lt 1) { return $null }
    return $rest.Substring(0, $cut)
}

# adb with a hard timeout: stdout as one string, or $null if it did not finish
# in time (the process is killed). `& $Adb ...` waits for ever, and this loop
# has no second thread: `adb push` of the seal or `adb mdns services` could
# hang the watcher for good on a device that accepts the connection and then
# says nothing. Each argument is quoted on its own, so a
# transport name with a space survives.
function Invoke-AdbTimeout {
    param([string[]]$Arguments, [int]$TimeoutSeconds = 20)
    $quoted = foreach ($a in $Arguments) {
        if ($a -match '[\s"]') { '"' + ($a -replace '"', '\"') + '"' } else { $a }
    }
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Adb
    $psi.Arguments = ($quoted -join ' ')
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $proc = $null
    try {
        $proc = [System.Diagnostics.Process]::Start($psi)
        $stdout = $proc.StandardOutput.ReadToEndAsync()
        $stderr = $proc.StandardError.ReadToEndAsync()
        if (-not $proc.WaitForExit($TimeoutSeconds * 1000)) {
            try { $proc.Kill() } catch {}
            Write-Log ("  adb {0} timed out after {1} s." -f ($Arguments -join ' '), $TimeoutSeconds)
            return $null
        }
        return $stdout.Result
    } catch {
        Write-Log ("  adb {0} failed: {1}" -f ($Arguments -join ' '), $_.Exception.Message)
        return $null
    } finally {
        if ($proc) { $proc.Dispose() }
    }
}

# Who actually answered at a freshly connected ip:port - `getprop ro.serialno`
# over the adb channel, the one identity a stranger on the Wi-Fi cannot put in
# an announcement. Run with a hard timeout, unlike the rest of this script: a
# phone that dropped off between the connect and this question can leave
# `adb shell` hanging, and this loop has to keep going. Anything that is not
# one bare token (error text, a timeout) is $null, never a serial to trust.
function Get-TransportSerial {
    param([string]$Target, [int]$TimeoutSeconds = 10)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Adb
    $psi.Arguments = "-s `"$Target`" shell getprop ro.serialno"
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $proc = $null
    try {
        $proc = [System.Diagnostics.Process]::Start($psi)
        $stdout = $proc.StandardOutput.ReadToEndAsync()
        $stderr = $proc.StandardError.ReadToEndAsync()
        if (-not $proc.WaitForExit($TimeoutSeconds * 1000)) {
            try { $proc.Kill() } catch {}
            Write-Log "  getprop at $Target timed out after $TimeoutSeconds s."
            return $null
        }
        $text = $stdout.Result.Trim()
        if (-not $text -or ($text -match '\s')) { return $null }
        return $text
    } catch {
        Write-Log "  getprop at ${Target} failed: $($_.Exception.Message)"
        return $null
    } finally {
        if ($proc) { $proc.Dispose() }
    }
}

# Which phone is behind each connected transport, as a serial -> transport map.
#
# "Is ANYTHING connected?" is not enough: with two phones on file and one of
# them still connected, the other could change IP or port and never be looked
# for again. The question has to be asked per serial, which is what
# Invoke-WatcherCycle does with this map.
#
# adb names a Wi-Fi transport by address, not by serial, so the serial is asked
# over the channel (`getprop ro.serialno`, the same identity the reconnect path
# already checks) - once per transport, then remembered while adb keeps listing
# it, and forgotten the moment it disappears. A transport that does not answer
# counts for nobody: its serial stays "missing", which at worst costs an extra
# mDNS look, never a seal or a disconnect on a guess; and it is asked again
# only every few cycles, so one hung phone cannot cost this loop its 10 s
# timeout on every tick.
$script:TransportSerials = @{}      # transport -> serial ($null while unanswered)
$script:TransportAskedAt = @{}      # transport -> cycle of the last question
$script:RetryIdentityEvery = 4

# When this watcher last sent a seal to each serial. A seal sent only on a
# (re)connect would never reach a phone that simply stays connected with the
# PC app closed, and the seal expires after 30 days. So every known serial
# that is connected gets a fresh seal once per SealRefreshHours - the first
# time at start-up, which is also what the app does when it connects.
$script:LastSealAt = @{}            # serial -> [datetime] of the last seal
$script:SealRefreshHours = 12
$script:LastPinCheckAt = @{}         # serial -> [datetime] of the last PIN protection check
$script:PinCheckMinutes = 5

function Resolve-ConnectedSerials {
    param([string[]]$Transports, [int]$Cycle)
    $present = @{}
    foreach ($t in $Transports) { $present[$t] = $true }
    foreach ($t in @($script:TransportSerials.Keys)) {
        if (-not $present.ContainsKey($t)) {
            $script:TransportSerials.Remove($t)
            $script:TransportAskedAt.Remove($t)
        }
    }
    $bySerial = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
    foreach ($t in $Transports) {
        $serial = $null
        if ($script:TransportSerials.ContainsKey($t)) { $serial = $script:TransportSerials[$t] }
        if (-not $serial) {
            $askedAt = $null
            if ($script:TransportAskedAt.ContainsKey($t)) { $askedAt = $script:TransportAskedAt[$t] }
            if (($null -eq $askedAt) -or (($Cycle - $askedAt) -ge $script:RetryIdentityEvery)) {
                $serial = Get-TransportSerial -Target $t
                $script:TransportSerials[$t] = $serial
                $script:TransportAskedAt[$t] = $Cycle
            }
        }
        if ($serial -and -not $bySerial.ContainsKey($serial)) { $bySerial[$serial] = $t }
    }
    return $bySerial
}

# Whether anything answers on ip:port, in 3 s. On Windows `adb connect` to a
# dead LAN address takes ~21 s (adb's own 10 s limit does not apply; the OS
# decides), and the saved address of a phone that moved is dead by definition,
# so every reconnect paid those seconds before trying the announced one. A
# phone that is up answers the SYN in milliseconds even when its screen is off.
# Anything that is not host:port (a USB serial, a name) is left to adb.
function Test-TcpReachable {
    param([string]$HostPort, [int]$TimeoutMs = 3000)
    $cut = $HostPort.LastIndexOf(':')
    if ($cut -lt 1) { return $true }
    $hostName = $HostPort.Substring(0, $cut)
    $port = 0
    if (-not [int]::TryParse($HostPort.Substring($cut + 1), [ref]$port)) { return $true }
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $task = $client.ConnectAsync($hostName, $port)
        if (-not $task.Wait($TimeoutMs)) { return $false }
        return [bool]$client.Connected
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

# One `adb connect`, with the probe above in front. Returns "connected",
# "already" (adb listed it before we dialled: not ours to judge - a getprop on
# it would be judging a transport somebody else opened, and a mismatch would
# disconnect it), "unreachable" or "failed".
function Connect-Transport {
    param([string]$Target, [string]$LogLine)
    if (-not (Test-TcpReachable -HostPort $Target)) {
        Write-Log "  $Target does not answer on its port (3 s) - not dialling it."
        return "unreachable"
    }
    Write-Log $LogLine
    $out = Invoke-AdbTimeout -Arguments @("connect", $Target) -TimeoutSeconds 20
    if ($null -eq $out) { return "failed" }
    $result = ($out -split "`r?`n") -join " "
    Write-Log "  -> $result"
    if ($result -match "already connected") { return "already" }
    if ($result -match "connected to") { return "connected" }
    return "failed"
}

# Why a candidate address is skipped when adb already lists it, or $null when
# it is free to dial. `offline` is the one state that is not skipped: it is the
# corpse a phone leaves behind when it moves, and `adb connect` to it answers
# "already connected to" without dialling anything, so the caller drops it
# first.
function Get-SkipReason {
    param([hashtable]$States, [string]$Candidate, [string]$Serial, [bool]$IsSaved)
    if (-not $States.ContainsKey($Candidate)) { return $null }
    $state = $States[$Candidate]
    if ($state -eq "offline") { return $null }
    $which = if ($Serial -eq "(targets.txt)") { "from targets.txt" } elseif ($IsSaved) { "saved address of $Serial" } else { "announced address of $Serial" }
    if ($state -eq "device") {
        $who = $null
        if ($script:TransportSerials.ContainsKey($Candidate)) { $who = $script:TransportSerials[$Candidate] }
        $inUse = if ($who) { "in use by $who" } else { "identity pending" }
        return "  $Candidate ($which) is already listed by adb - $inUse - not dialling."
    }
    return "  $Candidate ($which) is listed by adb as '$state' - leaving it alone."
}

# One pass of the loop, split out so it can be exercised by a harness against
# a fake adb without running forever. $Cycle only feeds the identity retry.
function Invoke-WatcherCycle {
    param([int]$Cycle)
    $states = Get-AdbTransportStates
    $transports = @()
    foreach ($t in $states.Keys) {
        if ($states[$t] -eq "device") { $transports += $t }
    }
    # Not wrapped in @(): Get-KnownSerials returns a Hashtable, and @() around
    # it would be a one-element array (.Count 1) even when empty.
    $knownSerials = Get-KnownSerials
    $connected = Resolve-ConnectedSerials -Transports $transports -Cycle $Cycle

    # Serials on file that no connected transport answered for. Two kinds of
    # row are not on this list: one with no serial (nothing to look for it
    # by), and one with a serial but no saved
    # address (a phone added over USB): there is no wireless address to go
    # back to, the app shows such a row as plain "Disconnected", and counting
    # it would keep this loop searching - and never saying OK - for as long as
    # that phone is unplugged. Both kinds are still dialled if they announce
    # themselves by mDNS while something else is being looked for -
    # and once such a phone has been reached over Wi-Fi and confirmed its
    # serial, its address is written down (Save-KnownAddress, the app does the
    # same) and from the next cycle on it counts as a Wi-Fi row like any other.
    $missing = @()
    foreach ($s in $knownSerials.Keys) {
        if ($null -eq $knownSerials[$s]) { continue }
        # "Forget Wi-Fi address" normally leaves the row without an address, so
        # the line above already covers it; a file edited by hand can have both,
        # and a serial that is never dialled must not be counted as missing
        # either - it would keep this loop from ever saying "OK".
        if ($script:CableOnlySerials.ContainsKey($s)) { continue }
        if (-not $connected.ContainsKey($s)) { $missing += $s }
    }
    $missing = @($missing | Sort-Object)
    $dialled = 0

    foreach ($s in @($connected.Keys)) {
        if (-not $knownSerials.ContainsKey($s)) { continue }
        # A phone on file that is connected at a plain ip:port other than the
        # one saved - the app or a hand-typed `adb connect` brought it here,
        # not this loop - gets that address written down too, so the next
        # search starts from where it really is. Only ip:port: an mDNS name is
        # not an address and USB has none.
        $at = $connected[$s]
        if (($at -match '^\d[\d.]*:\d+$') -and ($knownSerials[$s] -cne $at) -and
            (-not $script:CableOnlySerials.ContainsKey($s))) {
            Save-KnownAddress -Serial $s -HostPort $at
            $knownSerials[$s] = $at
        }
        $last = $null
        if ($script:LastSealAt.ContainsKey($s)) { $last = $script:LastSealAt[$s] }
        if (($null -eq $last) -or (((Get-Date) - $last).TotalHours -ge $script:SealRefreshHours)) {
            Send-Seal -Target $connected[$s]
            $script:LastSealAt[$s] = Get-Date
            Write-Log "Refreshed the seal on $s (at $($connected[$s]))."
        }
        $lastPin = $null
        if ($script:LastPinCheckAt.ContainsKey($s)) { $lastPin = $script:LastPinCheckAt[$s] }
        if (($null -eq $lastPin) -or (((Get-Date) - $lastPin).TotalMinutes -ge $script:PinCheckMinutes)) {
            $script:LastPinCheckAt[$s] = Get-Date
            Test-PinProtection -Serial $s -Target $connected[$s]
        }
    }

    if ($transports.Count -gt 0 -and $missing.Count -eq 0) {
        Write-Log "OK - device connected."
        return
    }
    if ($transports.Count -gt 0) {
        # Only phones on file are named; anything else adb lists (an emulator,
        # a phone never paired here) is a count, not a serial in the log.
        $onFile = @($connected.Keys | Where-Object { $knownSerials.ContainsKey($_) } | Sort-Object)
        $others = $connected.Count - $onFile.Count
        $have = $onFile -join ", "
        if ($others -gt 0) { $have = if ($have) { "$have (+$others not on file)" } else { "($others not on file)" } }
        if (-not $have) { $have = "(identity pending)" }
        Write-Log ("Connected: {0}. Missing: {1}. Searching via mDNS..." -f $have, ($missing -join ", "))
    } else {
        Write-Log "No devices. Searching via mDNS..."
    }

    # mDNS names are free text that anyone on the Wi-Fi can publish, so
    # they only say where to dial. Two gates before a phone found this
    # way gets a seal: its claimed serial is one the app has on file,
    # and once connected the device itself reports that same serial.
    # Neither gate touches targets.txt below: those are addresses the
    # user typed in by hand.
    if ($knownSerials.Count -eq 0) {
        if (($script:NoRegistryCycles % $NoRegistryWarnEvery) -eq 0) {
            Write-Log "No serials on file in devices.json - nothing found via mDNS will be dialled (targets.txt still applies). Pair the phone again from the Mirryl app to restore automatic reconnection."
        }
        $script:NoRegistryCycles++
    } else {
        $script:NoRegistryCycles = 0
        $mdnsRaw = Invoke-AdbTimeout -Arguments @("mdns", "services") -TimeoutSeconds 15
        if ($null -eq $mdnsRaw) { $mdnsRaw = "" }
        $mdnsRaw = $mdnsRaw -split "`r?`n"
        $matchingLines = $mdnsRaw | Where-Object { $_ -match "_adb-tls-connect\._tcp" }
        $ignoredThisCycle = @{}
        # Whoever publishes chooses how many lines carry a serial. The saved
        # address is dialled once per serial per cycle, and announced ones at
        # most three times: otherwise fifty announcements of one known serial
        # at dead addresses would keep this cycle busy for minutes.
        $savedTried = @{}
        $announcedTried = @{}
        $MaxAnnouncedPerSerial = 3

        foreach ($line in $matchingLines) {
            # "<instance>\t<type>\t<ip:port>" - TAB, like `adb devices`: the
            # instance name of a renamed announcement carries a space.
            $fields = $line.Trim() -split "`t"
            if ($fields.Count -lt 3) { continue }
            # Field 1 is the service type; the pairing service
            # (_adb-tls-pairing) lists the same phone on a port that
            # refuses ordinary connections.
            if ($fields[1].Trim().TrimEnd(".") -ne "_adb-tls-connect._tcp") { continue }
            $serial = Get-MdnsSerial -InstanceName $fields[0].Trim()
            if (-not $serial) { continue }
            if (-not $knownSerials.ContainsKey($serial)) {
                if (-not $ignoredThisCycle.ContainsKey($serial)) {
                    $ignoredThisCycle[$serial] = $true
                    Write-Log "Ignoring unknown device $serial advertised via mDNS."
                }
                continue
            }
            # Already here - on USB, or on another address that answered.
            if ($connected.ContainsKey($serial)) { continue }
            # On file, but the person pressed "Forget Wi-Fi address" for it.
            if ($script:CableOnlySerials.ContainsKey($serial)) {
                if (-not $script:CableOnlyLogged.ContainsKey($serial)) {
                    $script:CableOnlyLogged[$serial] = $true
                    Write-Log "  $serial is on file as cable-only - not dialling it (Forget Wi-Fi address)."
                }
                continue
            }
            $target = $fields[2].Trim()
            if ($target -notmatch ':') { continue }
            # The announced address is the LAST resort, not the first.
            # Matching the serial alone let anyone on the Wi-Fi who
            # copied it out of the phone's own multicast get dialled
            # here every cycle; the address on file for that serial is
            # not in the announcement, so try it first when it differs.
            # Still follows DHCP: if the saved address no longer
            # answers, the announced one is tried right after.
            $saved = $knownSerials[$serial]
            $candidates = @()
            if (($saved -is [string]) -and ($saved -match ':') -and ($saved -cne $target)) {
                if (-not $savedTried.ContainsKey($serial)) {
                    $savedTried[$serial] = $true
                    $candidates += $saved
                }
            }
            if (-not $announcedTried.ContainsKey($serial)) { $announcedTried[$serial] = 0 }
            if ($announcedTried[$serial] -lt $MaxAnnouncedPerSerial) {
                $announcedTried[$serial]++
                $candidates += $target
            } elseif ($announcedTried[$serial] -eq $MaxAnnouncedPerSerial) {
                $announcedTried[$serial]++
                Write-Log "  more than $MaxAnnouncedPerSerial announced addresses for $serial this cycle - ignoring the rest."
            }
            $connectedAt = $null
            foreach ($candidate in $candidates) {
                $skip = Get-SkipReason -States $states -Candidate $candidate -Serial $serial -IsSaved ($candidate -ceq $saved)
                if ($skip) { Write-Log $skip; continue }
                if ($states.ContainsKey($candidate)) {
                    Write-Log "  dropping offline transport at $candidate before dialling."
                    & $Adb disconnect $candidate *> $null
                    $states.Remove($candidate)
                }
                $dialled++
                $outcome = Connect-Transport -Target $candidate -LogLine "Connecting via mDNS: $candidate (claims serial $serial)"
                if ($outcome -eq "already") { $states[$candidate] = "device"; continue }
                if ($outcome -eq "connected") { $connectedAt = $candidate; break }
            }
            # The channel check applies to the address THIS cycle opened.
            if ($connectedAt) {
                $states[$connectedAt] = "device"
                $reported = Get-TransportSerial -Target $connectedAt
                if ($reported -cne $serial) {
                    Write-Log "  identity mismatch at ${connectedAt}: expected $serial, got '$reported' - disconnecting."
                    & $Adb disconnect $connectedAt *> $null
                    $states.Remove($connectedAt)
                    continue
                }
                # Remembered so the next cycle does not ask again.
                $script:TransportSerials[$connectedAt] = $reported
                $script:TransportAskedAt[$connectedAt] = $Cycle
                $connected[$serial] = $connectedAt
                # Where it lives now, for the next cycle and for the app.
                Save-KnownAddress -Serial $serial -HostPort $connectedAt
                Send-Seal -Target $connectedAt
                $script:LastSealAt[$serial] = Get-Date
            }
        }
    }

    # Hand-typed addresses have no serial to match, so they are dialled
    # whenever anything on file is missing - skipping the ones adb already
    # lists, which would only answer "already connected" and earn a seal per
    # cycle.
    if (Test-Path $TargetsFile) {
        $targets = Get-Content $TargetsFile | Where-Object { $_ -and -not $_.TrimStart().StartsWith("#") }
        foreach ($target in $targets) {
            $target = $target.Trim()
            if (-not $target) { continue }
            # A hand-typed address that belongs to a phone the person told us
            # to forget is still that phone. Dialling it here would undo the
            # forget every cycle, through the back door.
            $owner = $null
            foreach ($s in $script:CableOnlySerials.Keys) {
                if ($knownSerials[$s] -ceq $target) { $owner = $s; break }
            }
            # Learned the hard way in an earlier cycle: this address answered
            # as a phone on file as cable-only. Without remembering it, the
            # loop dials and drops it again every 25 s, for ever.
            if (-not $owner -and $script:CableOnlyTargets -and
                $script:CableOnlyTargets.ContainsKey($target) -and
                $script:CableOnlySerials.ContainsKey($script:CableOnlyTargets[$target])) {
                $owner = $script:CableOnlyTargets[$target]
            }
            if ($owner) {
                $logKey = "$target|$owner"
                if (-not $script:CableOnlyLogged.ContainsKey($logKey)) {
                    $script:CableOnlyLogged[$logKey] = $true
                    Write-Log "  $target is $owner, on file as cable-only - not dialling it (targets.txt)."
                }
                continue
            }
            $skip = Get-SkipReason -States $states -Candidate $target -Serial "(targets.txt)" -IsSaved $true
            if ($skip) { Write-Log $skip; continue }
            if ($states.ContainsKey($target)) {
                Write-Log "  dropping offline transport at $target before dialling."
                & $Adb disconnect $target *> $null
                $states.Remove($target)
            }
            $dialled++
            $outcome = Connect-Transport -Target $target -LogLine "Connecting via fixed IP: $target"
            if ($outcome -eq "already") { $states[$target] = "device"; continue }
            if ($outcome -eq "connected") {
                $states[$target] = "device"
                # Who answered? A hand-typed address carries no serial, so this
                # is the only place the loop can find out that it just dialled
                # a phone the person told us to forget - the usual case, where
                # the row has no saved address left to compare against.
                $answeredAs = Get-TransportSerial -Target $target
                if ($answeredAs -and $script:CableOnlySerials.ContainsKey($answeredAs)) {
                    Write-Log "  $target answered as $answeredAs, on file as cable-only - disconnecting it again (targets.txt)."
                    & $Adb disconnect $target *> $null
                    $states.Remove($target)
                    if ($null -eq $script:CableOnlyTargets) {
                        $script:CableOnlyTargets = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
                    }
                    $script:CableOnlyTargets[$target] = $answeredAs
                    continue
                }
                if ($answeredAs) {
                    $script:TransportSerials[$target] = $answeredAs
                    $script:TransportAskedAt[$target] = $Cycle
                }
                Send-Seal -Target $target
            }
        }
    }

    # A cycle that found nothing to dial already said who is missing; the
    # closing line is for what changed, plus the "nothing at all" case, which
    # keeps the log at two lines per cycle while the phone is away.
    $after = @(Get-ConnectedTransports)
    if ($after.Count -gt $transports.Count) {
        Write-Log "Successfully connected after retry."
    } elseif ($after.Count -eq 0) {
        Write-Log "Still no device. Retrying in $IntervalSeconds s."
    } elseif ($dialled -gt 0) {
        Write-Log ("Still missing: {0}. Retrying in {1} s." -f ($missing -join ", "), $IntervalSeconds)
    }
}

# One watcher per PC. Two would dial the same phones and fight over the same
# devices.json every cycle - and since the Store package starts one at every
# sign-in, and again right after pairing, a second launch is normal, not an
# accident. Global\ because the installer's task runs in session 0 and the
# Store one in the user's session; Local\ would not see the other. A mutex
# left abandoned by a watcher that was killed counts as ours.
$script:WatcherMutex = New-Object System.Threading.Mutex($false, "Global\Mirryl-Watcher")
$owned = $false
try { $owned = $script:WatcherMutex.WaitOne(0) } catch [System.Threading.AbandonedMutexException] { $owned = $true }
if (-not $owned) {
    Write-Log "Another watcher is already running - this one exits."
    exit 0
}

Write-Log "== Watcher started (interval: $IntervalSeconds s) =="
if ($script:EnvFilled.Count -gt 0) {
    Write-Log "Environment: Windows started this without $(($script:EnvFilled | ForEach-Object { ($_ -split '=')[0] }) -join ', '); filled in: $($script:EnvFilled -join '; ')"
}
& $Adb start-server *> $null
# "Nothing on file" is repeated roughly every 10 minutes, not logged once: a
# user whose rows have no `serial` would otherwise lose mDNS reconnection
# silently. Derived from the actual interval (24 cycles at the 25 s default)
# rather than a hardcoded count.
$NoRegistryWarnEvery = [Math]::Max(1, [int][Math]::Ceiling(600 / [Math]::Max(1, $IntervalSeconds)))
$script:NoRegistryCycles = 0
$cycle = 0

# An update replaces this file but not this process: the installer cannot stop
# or start the scheduled task without elevating (the user only has read access
# to it), and a non-zero exit does NOT make Task Scheduler restart it despite
# RestartCount 999. So the watcher notices its own file changed and runs the
# new one in this same process. The stamp is read with .NET only: a cmdlet such as
# Get-FileHash can be missing when PSModulePath is inherited from PowerShell 7,
# and a stamp that silently reads as $null would never change.
function Get-ScriptStamp {
    try {
        $f = New-Object System.IO.FileInfo($PSCommandPath)
        if (-not $f.Exists) { return $null }
        return ("{0}|{1}" -f $f.LastWriteTimeUtc.Ticks, $f.Length)
    } catch {
        return $null
    }
}
$script:ScriptStamp = Get-ScriptStamp
$script:PendingStamp = $null
if ($null -eq $script:ScriptStamp) {
    Write-Log "Cannot read the stamp of $PSCommandPath - an update of watcher.ps1 will not be picked up until a restart."
}

# True when the file changed and the new one is safe to run. The installer
# writes the file in place, so a cycle can land on half a script; a half that
# happens to parse could define the functions and return before the loop,
# leaving no watcher at all. Hence the new stamp has to be seen on two cycles
# in a row before it counts, and the file has to parse.
function Test-ScriptReplaced {
    if ($null -eq $script:ScriptStamp) { return $false }
    $now = Get-ScriptStamp
    if ($null -eq $now -or $now -eq $script:ScriptStamp) {
        $script:PendingStamp = $null
        return $false
    }
    if ($now -ne $script:PendingStamp) {
        $script:PendingStamp = $now
        return $false
    }
    $tokens = $null
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($PSCommandPath, [ref]$tokens, [ref]$errors)
    if ($errors -and $errors.Count -gt 0) {
        Write-Log ("watcher.ps1 changed on disk but does not parse ({0} errors) - keeping the running code." -f $errors.Count)
        $script:ScriptStamp = $now
        $script:PendingStamp = $null
        return $false
    }
    return $true
}

while ($true) {
    # The folder this script runs from can disappear under it. In the
    # Microsoft Store package that is every update: Windows installs the new
    # version beside the old one and deletes the old folder, but does not stop
    # this process. Left running, it would keep the mutex above and call an
    # adb.exe that no longer exists, so the new version's watcher would exit at
    # once. So it hands over: release the mutex, start the new version's
    # watcher, exit.
    if (-not (Test-Path -LiteralPath $ScriptDir)) {
        Write-Log "This watcher's folder is gone ($ScriptDir) - the app was updated or removed. Exiting."
        $script:WatcherMutex.ReleaseMutex()
        $script:WatcherMutex.Dispose()
        # Only a Store package has a successor to start: its folder is named
        # <name>_<version>_<arch>__<publisher id>, and the package manager knows
        # where the current version lives.
        $leaf = Split-Path $ScriptDir -Leaf
        if ($leaf -like "*__*") {
            $name = ($leaf -split "_")[0]
            $pkg = Get-AppxPackage -Name $name -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($pkg) {
                $exe = Join-Path $pkg.InstallLocation "Mirryl.exe"
                try {
                    Start-Process -FilePath $exe -ArgumentList "--watcher" -ErrorAction Stop
                    Write-Log "Started the watcher of $($pkg.PackageFullName)."
                } catch {
                    Write-Log "Could not start the watcher of $($pkg.PackageFullName): $($_.Exception.Message)"
                }
            }
        }
        exit 0
    }
    if (Test-ScriptReplaced) {
        Write-Log "watcher.ps1 changed on disk - reloading it in place."
        # Hand the mutex to the new code: it takes it again from this same
        # thread. Released first, not kept, so the new code's own release (on
        # the Store hand-over above) frees it for real instead of leaving one
        # count held by this frame.
        $script:WatcherMutex.ReleaseMutex()
        $script:WatcherMutex.Dispose()
        & $PSCommandPath -IntervalSeconds $IntervalSeconds -DataDir $DataDir -AdbPath $AdbPath
        exit 0
    }
    try {
        Invoke-WatcherCycle -Cycle $cycle
    } catch {
        # With the line: a bare "Cannot bind argument to parameter 'Path'
        # because it is null" says nothing about which of the dozen paths in a
        # cycle was the null one.
        $where = $_.InvocationInfo
        Write-Log "ERROR: $($_.Exception.Message) (line $($where.ScriptLineNumber): $($where.Line.Trim()))"
    }
    $cycle++
    Start-Sleep -Seconds $IntervalSeconds
}
