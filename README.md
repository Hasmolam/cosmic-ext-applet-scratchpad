# cosmic-ext-applet-scratchpad

Minimalist quick notes and scratchpad applet for the COSMIC Desktop environment.

Provides immediate access to three separate scratchpads directly from the COSMIC panel.

![COSMIC Scratchpad Applet](screenshots/screenshot_main.png)

## Features

- **Multi-Tab Workspace:** Three distinct pads (Notes, Snippets, Scratch) for temporary thoughts, shell commands, or draft text.
- **Crash-Resilient Atomic Storage:** Notes are stored as plain Markdown files (`pad_1.md`, `pad_2.md`, `pad_3.md`) under `$XDG_DATA_HOME/cosmic-scratchpad/`. All writes write to a PID-tagged temporary file first, fsync, and atomic rename.
- **Debounced Auto-Save:** Content saves automatically 400ms after typing stops, or immediately when closing the applet popup or switching tabs.
- **External Modification Sync:** Monitors file modification timestamps (`mtime`) and reloads external disk changes upon popup open or tab switch.
- **One-Click Copy:** Copies current tab text to the system clipboard with transient visual confirmation.
- **Non-Destructive Clear:** Clearing a note creates an atomic `.bak` backup file and displays a 5-second inline banner allowing immediate undo.
- **Word & Character Counter:** Real-time statistics displayed in the footer.
- **Settings Drawer:** In-app word wrap toggle and font size stepper (10pt - 24pt).
- **Native COSMIC Integration:** Uses `libcosmic` widgets, adaptive system theming, and `cosmic-config` (RON format).

## Storage & Synchronization

- **Note Data Directory:** `~/.local/share/cosmic-scratchpad/` (`pad_1.md`, `pad_2.md`, `pad_3.md`)
- **Configuration:** `~/.config/cosmic/io.github.hasmolam.cosmic-ext-applet-scratchpad/v1/config.ron`

Because notes are stored as standard Markdown files without database wrappers or proprietary schemas:
- **External Editing:** Files can be inspected or edited directly via terminal tools (`cat`, `nvim`, `nano`), GUI editors, or external note tools (Obsidian, VS Code).
- **External Sync:** The directory can be synchronized across machines using Syncthing, Git, Dropbox, or Nextcloud. External edits are detected and refreshed automatically when switching tabs or opening the popup.
- **Automated Backups:** Pre-clear operations write an atomic copy (`pad_<N>.md.bak`) before truncating the buffer.

## Keyboard Shortcuts

| Shortcut | Scope | Action |
|---|---|---|
| `Escape` | Popup / Editor | Closes the applet popover and flushes unsaved edits to disk |
| `Ctrl+Shift+C` | Popup / Editor | Copies the entire active pad buffer to the system clipboard |
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
