# CLI

```
strix [OPTIONS] [PATH]
```

## Arguments

| Argument | Description                                              |
|----------|----------------------------------------------------------|
| `PATH`   | Repository to open. Defaults to the current directory.   |

## Options

| Option           | Description                                                       |
|------------------|-------------------------------------------------------------------|
| `--theme <NAME>` | Theme to use for this run (overrides the config file).            |
| `--dump-frame`   | Render one frame to stdout as text, then exit (debugging aid).    |
| `--width <N>`    | Terminal width for `--dump-frame` (default 120).                  |
| `--height <N>`   | Terminal height for `--dump-frame` (default 40).                  |
| `--version`      | Print the version and exit.                                       |
| `--help`         | Print help and exit.                                              |

## Environment

| Variable    | Description                                                          |
|-------------|----------------------------------------------------------------------|
| `STRIX_LOG` | Log verbosity, same syntax as `RUST_LOG` (e.g. `info`, `debug`, `strix=trace`). Logs are written to a file, never to the terminal. |

## Logs

| Platform | Location                               |
|----------|----------------------------------------|
| macOS    | `~/Library/Logs/strix/strix.log`       |
| Linux    | `$XDG_STATE_HOME/strix/strix.log` (default `~/.local/state/strix/strix.log`) |
