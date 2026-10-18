# rustygb

A Game Boy (DMG) emulator written in simple, readable Rust.

## Features

- Complete Sharp LR35902 CPU, accurate to the machine cycle
- PPU with background, window and sprites, stepped one dot at a time
- Sound: both square channels, wave and noise, played through `cpal`
- Cartridges: ROM only, MBC1, MBC2, MBC3 (with real-time clock) and MBC5, with battery saves
- Keyboard and game controller input (`gilrs`)
- A ROM library picker

## Test results

| Suite | Result |
|---|---|
| Blargg `cpu_instrs`, `instr_timing`, `mem_timing` | all pass |
| Blargg `dmg_sound` | 9 / 12 |
| Mooneye acceptance (DMG) | 66 / 66 |
| Mooneye MBC1 / MBC2 / MBC5 | 12 / 13, 7 / 7, 8 / 8 |

## Running

```sh
cargo run --release -- path/to/game.gb        # play a ROM
cargo run --release -- --library path/to/roms # set the ROM folder and open the picker
cargo run --release                           # open the picker for the saved folder
```

Games started from the library keep their saves in the app data folder
(`~/Library/Application Support/rustygb/saves` on macOS). A ROM started
directly keeps its `.sav` file next to it.

## Controls

| Game Boy | Keyboard | Controller |
|---|---|---|
| D-pad | Arrow keys | D-pad or left stick |
| A | Z | South button (Xbox A, PlayStation Cross) |
| B | X | East button (Xbox B, PlayStation Circle) |
| Start | Enter | Start |
| Select | Backspace | Select |
| Back to library / quit | Esc | Home |

## Test ROM runners

```sh
cargo run --release --example headless -- test.gb   # prints serial output (Blargg tests)
cargo run --release --example mooneye -- tests/*.gb # pass/fail for Mooneye tests
```

No ROMs are included. You need your own game or test ROM files.

## License

MIT, see [LICENSE](LICENSE).
