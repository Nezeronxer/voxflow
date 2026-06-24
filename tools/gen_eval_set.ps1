# Generate synthetic RU/EN eval set via SAPI (System.Speech), 16 kHz mono WAV + .ref.txt.
# ASCII-only script; phrases live in eval_phrases.json (UTF-8) to dodge codepage traps.
param([string]$OutDir = "$env:LOCALAPPDATA\VoxFlow\eval")

Add-Type -AssemblyName System.Speech
$phrasesPath = Join-Path $PSScriptRoot "eval_phrases.json"
$json = [System.IO.File]::ReadAllText($phrasesPath, [System.Text.Encoding]::UTF8)
$data = $json | ConvertFrom-Json
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(
    16000,
    [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
    [System.Speech.AudioFormat.AudioChannel]::Mono)

foreach ($lang in @("ru", "en")) {
    $culture = if ($lang -eq "ru") { "ru-RU" } else { "en-US" }
    $voice = $synth.GetInstalledVoices() |
        Where-Object { $_.VoiceInfo.Culture.Name -eq $culture -and $_.Enabled } |
        Select-Object -First 1
    if (-not $voice) { Write-Output "NOVOICE $culture"; continue }
    $synth.SelectVoice($voice.VoiceInfo.Name)
    Write-Output ("VOICE " + $lang + ": " + $voice.VoiceInfo.Name)
    $i = 0
    foreach ($p in $data.$lang) {
        $i++
        $name = "{0}_{1:d2}" -f $lang, $i
        $wav = Join-Path $OutDir ($name + ".wav")
        $synth.SetOutputToWaveFile($wav, $fmt)
        $synth.Speak($p)
        $synth.SetOutputToNull()
        [System.IO.File]::WriteAllText(
            (Join-Path $OutDir ($name + ".ref.txt")), $p,
            (New-Object System.Text.UTF8Encoding($false)))
        Write-Output ("WROTE " + $name)
    }
}
$synth.Dispose()
Write-Output "DONE"
