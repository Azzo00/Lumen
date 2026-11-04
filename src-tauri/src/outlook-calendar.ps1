# Reads the default calendar of classic Outlook through COM and prints the events as JSON.
# Used by Lumen (agenda.rs) when the calendar source is "Outlook classico".
# If Outlook was not running, it is started invisibly, given a few seconds to sync
# with Exchange, and closed again - unless the user opened a window in the meantime.
param([int]$DaysBack = 40, [int]$DaysAhead = 62, [int]$SyncSeconds = 25)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$wasRunning = [bool](Get-Process OUTLOOK -ErrorAction SilentlyContinue)
$ol = $null
$out = New-Object System.Collections.ArrayList
try {
    $ol = New-Object -ComObject Outlook.Application
    $ns = $ol.GetNamespace('MAPI')
    if (-not $wasRunning -and $SyncSeconds -gt 0) {
        try { $ns.SendAndReceive($false) } catch {}
        Start-Sleep -Seconds $SyncSeconds
    }
    $items = $ns.GetDefaultFolder(9).Items          # 9 = olFolderCalendar
    $items.Sort('[Start]')
    $items.IncludeRecurrences = $true
    $from = (Get-Date).Date.AddDays(-$DaysBack)
    $to = (Get-Date).Date.AddDays($DaysAhead)
    # Restrict wants dates in the user's regional format
    $filter = "[Start] < '" + $to.ToString('g') + "' AND [End] > '" + $from.ToString('g') + "'"
    $res = $items.Restrict($filter)

    $re = [regex]'https://(teams\.microsoft\.com/(l/meetup-join|meet)/|teams\.live\.com/|[\w.-]*zoom\.us/j/|meet\.google\.com/)[^\s<>"\)\]]+'
    $toMs = { param($utc) [DateTimeOffset]::new([DateTime]::SpecifyKind($utc, 'Utc')).ToUnixTimeMilliseconds() }
    $a = $res.GetFirst()
    $n = 0
    while ($null -ne $a -and $n -lt 800) {
        $n++
        $skip = ($a.MeetingStatus -eq 5 -or $a.MeetingStatus -eq 7 -or $a.ResponseStatus -eq 4)  # cancelled / declined
        if (-not $skip) {
            $join = $null
            $m = $re.Match([string]$a.Location)
            if ($m.Success) { $join = $m.Value }
            if (-not $join) {
                try { $m = $re.Match([string]$a.Body); if ($m.Success) { $join = $m.Value } } catch {}
            }
            $start = & $toMs $a.StartUTC
            [void]$out.Add([ordered]@{
                id       = "$($a.GlobalAppointmentID)#$start"
                title    = [string]$a.Subject
                start    = $start
                end      = & $toMs $a.EndUTC
                allDay   = [bool]$a.AllDayEvent
                location = [string]$a.Location
                join     = $join
                free     = ($a.BusyStatus -eq 0)                            # 0 = olFree
            })
        }
        [void][Runtime.InteropServices.Marshal]::ReleaseComObject($a)
        $a = $res.GetNext()
    }
}
finally {
    if ($ol -and -not $wasRunning) {
        try { if ($ol.Explorers.Count -eq 0 -and $ol.Inspectors.Count -eq 0) { $ol.Quit() } } catch {}
    }
}
ConvertTo-Json -InputObject @($out) -Depth 3 -Compress
