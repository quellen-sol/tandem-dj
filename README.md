# Tandem

Back-to-back DJing with a friend, from anywhere. Two DJs in different places share two decks and a mixer, each hears the full mix at full quality in their own booth, and one of you can stream the set.

**Personal, non-commercial use only.** See [TERMS.md](TERMS.md).

> **Status: v0.1, first round.** Windows only. The sync core is proven by tests; the rest is a playable first version for two people to try. See [docs/ROADMAP.md](docs/ROADMAP.md) for what's next and a test plan.

## How it works

Both laptops run the whole mix locally. Only small, timestamped control messages ("deck A play at 12:04:31.208") cross the internet, and both machines agree on one shared clock. The same moves at the same timestamps give the same mix on both laptops.

- **Your own controls are instant.** Your partner's moves reach you about half a round trip late, typically 20–80 ms.
- **The broadcast is exact.** The streaming feed runs half a second behind, so both DJs' moves land in it exactly where they happened.
- **Tracks are matched by content.** If your partner loads a track you already have, Tandem finds your copy. If you don't have it, it's copied over for the session and deleted when Tandem closes.

The details are in [docs/DESIGN.md](docs/DESIGN.md).

## Install

1. Download `tandem-windows-x64.zip` from [Releases](../../releases) and unzip it anywhere.
2. Run `tandem.exe`. Windows SmartScreen will warn because the app isn't signed: click **More info → Run anyway**.
3. Enter your DJ name and accept the terms.

To check a PC without opening the app, open a terminal in the folder and run:

```
tandem.exe --self-test
```

The self-test lists your audio and MIDI devices, opens your default output for half a second of silence, and runs a full two-booth test over the network stack. It ends with `SELF-TEST PASSED`.

## Set up your booth

1. **Music.** In the library, click **Add music folder…**. Serato crates from `Music\_Serato_` (and `X:\_Serato_` on external drives) show up automatically. Tandem only reads them and never writes Serato files.
2. **Audio.** Open **Settings → Audio**.
   - **Speakers (master):** your controller's sound card, channels 1/2.
   - **Headphones (cue):** the same device, channels 3/4. Most controllers expose four channels.
   - Use the **🎧 CUE** buttons on the mixer to send a deck to your headphones, and **phones: cue ↔ master** to blend.
3. **Controller.** Close Serato, rekordbox and any other DJ app, because only one app can use the controller at a time. Then open **Settings → Controller**, pick your controller, click **Learn** next to each function and move or press the matching control.
   - **Faders and knobs use soft takeover.** A hardware fader only takes over once it reaches the value the room already has, so you never jump over your partner's mix.
   - **Jog wheel moves the wrong way or jumps?** Tick **1/127 style** or **invert** next to it.

Mappings are saved per controller, so you only do this once.

## Play together

1. **Get connected.** The easiest way is for both of you to install [Tailscale](https://tailscale.com) (free) and sign in. You'll each get a `100.x.y.z` address that works from anywhere. Without it, the host has to forward TCP port 47800 on their router.
2. **Host.** One of you clicks **Host a room**, then sends the other one of the listed addresses and the room code. Click **Allow** if Windows Firewall asks.
3. **Join.** The other clicks **Join a room**, enters the address and code, and joins.
4. **Check the connection.** The top bar shows your partner, the round-trip time and **✔ in sync**.

**Sync click** in the top bar plays a metronome on the shared room clock. Put both laptops side by side, or on a phone call, and the clicks should line up.

### Sharing crates

1. Pick a folder or Serato crate.
2. Click **Share this list…** and confirm you have the right to share it.
3. Your partner gets a popup and must agree to use the tracks for personal, non-commercial purposes before anything downloads.
4. Tracks they already have are skipped. The rest are saved to `Music\Tandem Shared\<your name>\<crate>` and show up under **Shared with me**.

Every agreement is written to a local consent log (Settings → General → Logs and consent record).

### Streaming the set

1. On the broadcaster's PC, install [VB-Audio Virtual Cable](https://vb-audio.com/Cable/).
2. Set **Settings → Audio → Broadcast feed** to **CABLE Input**.
3. In OBS, add an *Audio Input Capture* of **CABLE Output**.

The broadcast feed is delayed by 500 ms by default, so both DJs' moves are on time in it. If you add a camera in OBS, delay the video by the same amount.

## Trying it on one PC

Run a second, completely separate copy with its own settings:

```
tandem.exe --data-dir C:\temp\tandem-bob
```

Host in one window and join `127.0.0.1` from the other.

## Known limits in v0.1

- **Decks and DJs:** 2 decks, 2 DJs per room.
- **No key lock:** tempo changes pitch, like vinyl.
- **No beat sync yet:** no beatgrids, sync button or quantize. BPM comes from file tags (Serato writes these).
- **Mixer:** no effects or loops. EQ is a 3-band isolator.
- **Controllers:** MIDI only, with no LED feedback yet. Controllers that only speak HID aren't supported.
- **Audio driver:** WASAPI shared mode, roughly 10–20 ms of output latency. No ASIO yet.
- **Serato data:** crates are read, but Serato cue points and beatgrids aren't imported yet.
- **Security:** traffic isn't encrypted by Tandem itself. Use Tailscale, which is encrypted. The room code is a simple gate, not a password.

## Troubleshooting

- **Can't connect.**
  - Check that the address and room code are right.
  - The host must click **Allow** on the Windows Firewall prompt (or allow `tandem.exe` in firewall settings).
  - Without Tailscale, the host needs TCP 47800 forwarded.
- **Controller not listed.** Close other DJ software, replug it, then click **Refresh**.
- **No sound.** Pick the right devices in **Settings → Audio** and check **Status** there.
- **Logs.** `%LOCALAPPDATA%\Tandem\data\tandem.log`. Settings → General has buttons to open every folder.

## Build from source

You need Rust (stable) and the Visual Studio Build Tools (C++ workload).

```
cargo build --release -p tandem      # target\release\tandem.exe
cargo test -p tandem-core            # core tests, including a two-booth TCP test
```

`tandem-core` is platform-independent and its tests also run on Linux and macOS. Windows builds and releases are produced by GitHub Actions (`.github/workflows/build.yml`). Push a `v*` tag to publish a release.

## License

[PolyForm Noncommercial 1.0.0](LICENSE). The source is available for any non-commercial use. This is intentionally *not* an OSI open-source license, because those can't forbid commercial use.
