# cosmic-ext-applet-scratchpad

Minimalist quick notes and scratchpad applet for the COSMIC Desktop environment.

Provides immediate access to three separate scratchpads directly from the COSMIC panel.

![COSMIC Scratchpad Applet](screenshots/screenshot_main.png)

## Features

- **Dynamic Note Navigation:** Sequential navigation (`<` Previous, `>` Next, `+` New Note) supporting arbitrary numbers of notes (`pad_N.md`), maintaining backward compatibility with existing 3-pad setups.
- **Notational Velocity Search & Instant Create:** Integrated live search (`Ctrl+F` / `Ctrl+K` or clicking search / note title). Filters note titles and body content in real time. Pressing `Enter` or clicking create instantiates a new note immediately with the query as title.
- **Title Derivation:** Dynamically derives note title from the first non-empty Markdown header or text line without requiring a database.
- **Crash-Resilient Atomic Storage:** Notes are stored as plain Markdown files (`pad_N.md`) under `$XDG_DATA_HOME/cosmic-scratchpad/`. All writes write to a PID-tagged temporary file first, fsync, and atomic rename.
- **Non-Destructive Delete & Undo:** Deleting a note moves it safely to the `trash/` subdirectory and displays a 5-second inline banner allowing immediate undo.
- **Debounced Asynchronous Auto-Save:** Content saves asynchronously in a background thread 400ms after typing stops, or immediately when closing the popup or switching notes without UI stutter.
- **External Modification Sync:** Monitors file modification timestamps (`mtime`) and reloads external disk changes upon popup open.
- **One-Click Copy:** Copies current note text to the system clipboard with transient visual confirmation (`Ctrl+Shift+C`).
- **Word & Character Counter:** Real-time statistics displayed in the footer.
- **Settings Drawer:** In-app word wrap toggle and font size stepper (10pt - 24pt).
- **Native COSMIC Integration:** Uses `libcosmic` widgets, adaptive system theming, and `cosmic-config`.

## Storage & Synchronization

- **Note Data Directory:** `~/.local/share/cosmic-scratchpad/` (`pad_1.md`, `pad_2.md`, ...)
- **Trash Directory:** `~/.local/share/cosmic-scratchpad/trash/`
- **Configuration:** `~/.config/cosmic/io.github.hasmolam.cosmic-ext-applet-scratchpad/v1/config.ron`

Because notes are stored as standard Markdown files without database wrappers or proprietary schemas:
- **External Editing:** Files can be inspected or edited directly via terminal tools (`cat`, `nvim`, `nano`), GUI editors, or external note tools (Obsidian, VS Code).
- **External Sync:** The directory can be synchronized across machines using Syncthing, Git, Dropbox, or Nextcloud. External edits are detected and refreshed automatically when opening the popup.
- **Safety:** Deleted notes are preserved under `trash/`.

## Keyboard Shortcuts

| Shortcut | Scope | Action |
|---|---|---|
| `Ctrl+F` / `Ctrl+K` | Popup / Editor | Toggle Notational Velocity search bar |
| `Ctrl+N` | Popup / Editor | Create a new note |
| `Ctrl+[` / `Alt+Left` | Popup / Editor | Switch to previous note |
| `Ctrl+]` / `Alt+Right` | Popup / Editor | Switch to next note |
| `Ctrl+Shift+C` | Popup / Editor | Copy active note buffer to system clipboard |
| `Escape` | Search View | Exit search and return to editor |
| `Escape` | Editor | Close applet popover and flush unsaved edits |
| Standard navigation | Editor | Text motions (`Ctrl+A`, `Ctrl+Z`, `Ctrl+C`, `Ctrl+V`, cursor keys) |

## Installation

### Method 1: Single-Line Quick Install

Downloads the prebuilt release binary, installs desktop entry and icons, and reloads the panel (no root required):

```bash
curl -fsSL https://raw.githubusercontent.com/Hasmolam/cosmic-ext-applet-scratchpad/main/install.sh | bash
```

### Method 2: Arch Linux (PKGBUILD)

```bash
git clone https://github.com/Hasmolam/cosmic-ext-applet-scratchpad.git
cd cosmic-ext-applet-scratchpad/packaging/arch
makepkg -si
```

### Method 3: Build from Source

#### Build Dependencies (Ubuntu/Pop!_OS)

```bash
sudo apt-get update
sudo apt-get install -y cmake libexpat1-dev libfontconfig-dev libfreetype-dev libxkbcommon-dev libwayland-dev pkg-config just
```

#### Compiling and Installing

```bash
git clone https://github.com/Hasmolam/cosmic-ext-applet-scratchpad.git
cd cosmic-ext-applet-scratchpad

# System-wide installation (installs to /usr, requires sudo):
sudo just install

# Or user-local installation (installs to ~/.local, no root required):
just install-user
```

### Adding to the Panel

In COSMIC Desktop settings:
`Settings -> Desktop -> Panel -> (Select your Top or Bottom Panel) -> Applets -> Add Applet -> "Scratchpad"`

## Troubleshooting

### Applet does not appear in "Add Applet" list
1. Ensure `~/.local/bin` is present in your environment `PATH`:
   ```bash
   echo $PATH | grep -q "$HOME/.local/bin" && echo "OK" || echo "Missing ~/.local/bin"
   ```
   If missing, add it to your shell configuration (`~/.bashrc`, `~/.zshrc`, or `~/.profile`):
   ```bash
   export PATH="$HOME/.local/bin:$PATH"
   ```
2. Re-register the desktop application cache:
   ```bash
   update-desktop-database ~/.local/share/applications
   ```
3. Restart the COSMIC panel process:
   ```bash
   killall cosmic-panel
   ```

## Uninstallation

If installed via the one-line quick installer:

```bash
curl -fsSL https://raw.githubusercontent.com/Hasmolam/cosmic-ext-applet-scratchpad/main/uninstall.sh | bash
```

Or from a local repository clone:

```bash
sudo just uninstall
# or for user-local install:
just uninstall-user
```

*(Note: User notes in `~/.local/share/cosmic-scratchpad/` are preserved).*

## Development

```bash
# Check formatting, lints, and unit tests
just check

# Run directly
just run
```

## License

GPL-3.0-only
