---
name: drawer
description: A machine room you browse like a library card catalog, drawn entirely in your terminal's own colours.
colors:
  # Every value is the terminal's own: "default" = the terminal's fg/bg,
  # "ansi N" = slot N of the active terminal theme (0-15).
  ground: "default background"
  row-selected: "ansi 8 (bright black) background"
  text: "default foreground"
  secondary: "ansi 7 (white)"
  faint: "ansi 8 (bright black)"
  rule: "ansi 8 (bright black)"
  focus-rule: "ansi 4 (blue)"
  live: "ansi 3 (yellow)"
  stamp: "ansi 5 (magenta)"
  on-tab: "ansi 0 (black)"
  ok: "ansi 2 (green)"
  err: "ansi 1 (red)"
  machine-tabs: "ansi 4, 5, 6, 2, 3, 12, 13, 14, 10, 11"
typography:
  title:
    fontFamily: "terminal monospace (Nerd Font)"
    fontSize: "1 cell"
    fontWeight: 700
    letterSpacing: "1 cell between letters, uppercase"
  name:
    fontFamily: "terminal monospace (Nerd Font)"
    fontSize: "1 cell"
    fontWeight: 700
  body:
    fontFamily: "terminal monospace (Nerd Font)"
    fontSize: "1 cell"
    fontWeight: 400
  meta:
    fontFamily: "terminal monospace (Nerd Font)"
    fontSize: "1 cell"
    fontWeight: 400
    fontVariation: "italic"
  label:
    fontFamily: "terminal monospace (Nerd Font)"
    fontSize: "1 cell"
    fontWeight: 700
rounded:
  none: "0"
spacing:
  cell: "1 col"
  hint-gap: "3 col"
  action-gap: "5 col"
  drawer-pitch: "3 row"
  sidebar: "28 col"
  sidebar-narrow: "24 col"
  files-xl: "46 col"
  files-lg: "42 col"
  files-md: "38 col"
  files-sm: "34 col"
components:
  pane:
    backgroundColor: "{colors.ground}"
    textColor: "{colors.text}"
    rounded: "{rounded.none}"
  pane-title:
    textColor: "{colors.secondary}"
    typography: "{typography.title}"
  pane-title-focused:
    textColor: "{colors.text}"
    typography: "{typography.title}"
  row-selected:
    backgroundColor: "{colors.row-selected}"
    textColor: "{colors.text}"
  row-selected-focused:
    backgroundColor: "{colors.row-selected}"
    textColor: "{colors.live}"
  key-hint:
    textColor: "{colors.live}"
    typography: "{typography.label}"
  key-hint-disabled:
    textColor: "{colors.faint}"
    typography: "{typography.meta}"
  popup-card:
    backgroundColor: "{colors.ground}"
    textColor: "{colors.text}"
    rounded: "{rounded.none}"
  date-due-stamp:
    textColor: "{colors.stamp}"
    typography: "{typography.label}"
    width: "15 col"
    height: "4 row"
  shell-tab:
    textColor: "{colors.on-tab}"
    typography: "{typography.name}"
    padding: "0 1 col"
---

# Design System: drawer

> Platform note: drawer is a full-screen terminal UI (Rust + ratatui, crossterm backend), not a web surface. It uses **no fixed colours**: every colour is either the terminal default or one of the terminal theme's 16 ANSI slots, so it matches whatever terminal/Omarchy theme is active and follows theme switches live. Every measure is in terminal cells. There is one typeface, the user's terminal monospace (a Nerd Font is assumed); hierarchy comes from weight, italic, case, tracking and colour. There is no motion, no radius and no shadow.

## Overview

**Creative North Star: "The Card Catalog Cabinet"**

A machine room you browse like a library card catalog: machines are drawers with coloured guide tabs, files are catalog cards in tidy columns, and a download is a checkout whose slip gets a DATE DUE stamp that stays on the file's row. The catalog lives in structure, not paint: the owner asked for the app to match the terminal completely, so the palette is the terminal theme itself.

Colour does jobs, not decoration. Default, white and bright black carry the text hierarchy. A blue top rule marks the one focused pane. Yellow is reserved for what can be acted on right now. Each machine keeps one accent across its sidebar tab, shell tab and files breadcrumb. State changes are instant steps; nothing eases.

**Key Characteristics:**
- Terminal background everywhere; panes separated by bright-black hairlines, not fills.
- A heavy blue top rule only on the focused pane and on popup cards.
- Yellow only on live keys and the focused selection.
- One stable accent per machine, hashed from its alias.
- Uppercase letter-spaced pane titles, like call numbers on a drawer label.
- Metadata in faint italic, numbers right-aligned in fixed-width columns.
- The magenta date-due stamp as the signature mark.

## Colors

All roles map to terminal slots (see frontmatter). Changing the terminal theme recolours drawer with it.

### Primary
- **Live** (ansi 3): key letters in every hint, the selected machine and file name while their pane has focus, the active form field and its underline. Nothing else.

### Secondary
- **Focus Rule** (ansi 4): the heavy top edge (`┍━━┑`) of the focused pane and of every popup card. Never text, never fill.
- **Stamp** (ansi 5): the stamped DATE DUE box, the `✓10 NOV` mark on downloaded rows, the progress fill, the footer transfer readout. It means "this was downloaded" and nothing else.

### Tertiary
- **Machine Tabs** (ansi 4, 5, 6, 2, 3 and bright 12, 13, 14, 10, 11; red is kept for errors): one per machine by FNV-1a hash of the alias. Used as the `▌` tab bar, the shell tab (text in ansi 0 on it), the files breadcrumb marker, and the alias in popups.
- **OK** (ansi 2) and **Error** (ansi 1): connection dot, saved toast; failures, `✕`, "connection dropped", the delete key in the delete confirmation, form errors.

### Neutral
- **Ground** (default bg): screen, panes and popups. The remote shell's own colours pass through untouched.
- **Selected Row** (ansi 8 bg): selection fill for drawers and file rows.
- **Text** (default fg): primary text and names.
- **Secondary** (ansi 7): hint labels, unfocused titles, sizes, icons; the readable text colour on a selected row.
- **Faint** (ansi 8): metadata in italic, counts, disabled keys, borders. Never drawn on a selected row.

### Named Rules
**The Brass Is Live Rule.** Brass means "press this now". A key whose action cannot run right now (for example `d` on a folder) drops to a faint key and a faint italic label.

**The One Hue Per Machine Rule.** A machine's tab hue is the same in the sidebar, the shell tab, the files breadcrumb and any popup that names it. Never assign hues by list position.

**The Faint Never Selects Rule.** Faint (ansi 8) is the selected-row fill, so any faint element inside a selected row switches to secondary (ansi 7).

**The Held Alarm Rule.** A drawer that dropped or failed keeps its error-coloured message in place of the address until the machine is selected.

## Typography

**Display Font:** none (terminal monospace only)
**Body Font:** the user's terminal monospace, Nerd Font expected
**Label/Mono Font:** same

**Character:** One cell, one face. Hierarchy is carried by weight, italic, case, tracking and ink.

### Hierarchy
- **Title** (bold when focused, uppercase, one space between letters): pane titles and popup titles (`M A C H I N E S`, `F I L E S`, `D O W N L O A D`). Default-foreground bold when focused, secondary regular when not. Kept call-number short.
- **Name** (bold): machine aliases, folder names, popup subject lines, help section heads. Folders and symlinked folders are bold; files are regular; symlinks add italic.
- **Body** (regular): paths, explanatory lines, the shell.
- **Meta** (italic, faint): addresses, modified dates, "from", remote paths, "any key to close".
- **Label** (bold): key letters in hints (live yellow), the DATE DUE caption (secondary), the date-due stamp (stamp).

### Named Rules
**The Call Number Rule.** Only pane and popup titles are letter-spaced uppercase. Body copy, labels and names stay in sentence or source case.

**The Tabular Column Rule.** Sizes and dates sit right-aligned in fixed-width columns (size 5 col, date 6 col, stamp 7 col); metadata is columns, never prose.

## Layout

Three columns on the terminal background above a one-row key legend.

- **Machines** on the left: 28 col at 110+ wide, 24 col below.
- **Shell** in the centre: takes the remaining width.
- **Files** on the right: 46 col at 150+, 42 at 140-149, 38 at 120-139, 34 at 100-119.
- **Below 100 col** the files pane replaces the shell while it has focus; only two columns show.
- **Below 60 × 14** the screen shows a centred "drawer needs a bigger window" notice and nothing else.
- **Footer** is one row on the terminal background: key hints on the left separated by 3 col, status (toast or transfer) right-aligned. Hints are dropped whole from the end until both fit; never truncated mid-hint.
- **Drawers** are two rows (alias plus state glyph, then address) with one row of air between, so a 3-row pitch.
- **Files pane** stacks breadcrumb row, ruled hairline row, list, and a status row at the bottom (`3 items · 1 hidden .`). The date-due stamp column is reserved only in folders that hold a downloaded file, so other folders keep the full name width.
- **Popups** are centred cards (60 × 13 for download and transfer, 62 wide for help, 48 × 8 for confirms) with 2 col inner side padding and 1 row top and bottom. Actions sit on the bottom row, 5 col apart.

## Elevation & Depth

No shadows and no tonal fills: everything sits on the terminal background. Depth comes from borders and the focus rule; selection is the one fill (ansi 8). Popups clear the cells beneath them and carry their own blue focus rule; nothing else floats.

### Named Rules
**The Instant Step Rule.** No easing, fades or animated transitions. State changes redraw in one step.

## Shapes

Square box-drawing only. Light single lines (`┌─┐│`) for every border; the focused pane's top edge is replaced with heavy line and heavy-down corners (`┍━┑`) in red-rule. The empty DATE DUE field is dashed (`┌┄┐ ┆`) in rule colour; once stamped it becomes a double-ruled box (`╔═╗ ║`) in stamp magenta. The guide tab is a left half block (`▌`). No rounded corners anywhere.

## Components

### Pane (catalog card)
- **Corner Style:** square light box-drawing.
- **Background:** panel.
- **Border:** rule hairline on all sides; focused pane gets the focus-rule heavy top edge and an default-foreground bold title.
- **Internal Padding:** content starts at the inner edge; text-only notes inset 1 col.

### Machine Drawer (signature)
Two rows on a 3-row pitch. Row 1: `▌` in machine hue, space, bold alias (live yellow when selected and focused), state glyph at the right edge (`●` ok, `◌` default foreground connecting, `✕` err, `○` faint ended). Row 2: `▌` again, address in faint italic (secondary italic when selected), or the held alarm in err. Selection fills both rows with row-selected. A faint count line (`4 machines · 1 open`) closes the pane.

### Shell Tab
The shell pane's title is a solid chip in the machine's hue with the alias in bold ansi-0 text, padded 1 col each side, followed by a state word (secondary "connecting", faint "ended", err "couldn't connect") when not connected. Scrollback shows `↑ N lines` in secondary at the right of the top border. A dead session shows a reverse-free bar on the bottom row with the message and `enter`/`x` hints.

### File Row (catalog card)
`icon name … stamp size date`. Icon in secondary (Nerd Font folder, file, link glyphs; `/`, space, `@` when `DRAWER_NO_ICONS` is set). Folders get a trailing `/`. Size in secondary, compact (`47K`); date in faint italic. Selected: row-selected fill, name in live yellow when focused.

### Key Hint (navigation)
- **Style:** key in live yellow bold, one space, label in secondary.
- **Disabled:** key in faint, label in faint italic.
- **Spacing:** 3 col between hints in the footer, 5 col between popup actions.

### Checkout Slip (download popup, signature)
Card titled `D O W N L O A D`. Left: bold file name; size in secondary, "from" in faint italic, alias in its machine hue bold; remote path in faint italic, clipped from the left. Right: the DATE DUE field, caption in secondary bold above a 15 × 3 dashed rule-coloured box. Below: "Saves to" in secondary and the local path in the default foreground. Bottom row: `y download` and `n cancel`.

### Date-Due Stamp (signature)
On `y`, the transfer card shows the DATE DUE box double-ruled in stamp magenta with the date centred (`10 NOV 2026`), and progress as a heavy magenta line (`━`) filling a light rule line (`─`), with secondary stats below. After the download, that file's row carries `✓10 NOV` in bold stamp magenta for the session.

### Status Line and Toast
Right side of the footer. Toasts take the tone colour (ok, err, default foreground); an active transfer shows `↓ name 42%` in stamp magenta, with `+N` for more.

### Machine Form (add / edit)
Card titled `A D D   M A C H I N E` or `E D I T   M A C H I N E`. Five labelled fields (Name, Address, User, Port, Key file), each on a ruled underline; the active field's label and underline are live yellow with a `▏` caret, empty fields show a faint italic hint. "Saves to" names the exact config file. Validation errors appear in red bold above `tab next field · ctrl+s save · esc cancel`.

### Delete Confirmation
Names the machine (in its hue) and the exact file, states that a `.drawer.bak` backup is kept. The confirming `y` is drawn in error red, not yellow, and Enter never confirms.

### Upload Picker
Destination (`alias:/remote/dir`) on top, a single path field with yellow underline and caret, up to six faint italic completions below, `tab complete · enter upload · esc cancel`.

### Welcome Tour (onboarding, signature)
A full-screen card shown on first launch (`drawer --welcome` or `w` in help repeats it), three steps with `●○○` progress and `← back · → next · esc skip`:
- **Welcome:** a filing cabinet drawn in box-drawing characters, with the middle drawer (live yellow) pulled out past the frame and index cards inside carrying the user's first two machines in their tab colours. Beside it, the lowercase block-letter wordmark and a two-line description.
- **Your setup:** live checks with `✓` (green) or `!` (yellow): the OpenSSH version, the machine count and source file with tabs, whether there's an SSH key (with the `K` tip, or the `ssh-keygen` command), and the Downloads folder.
- **How it works:** a miniature of the three panes, each with its keys and its `alt+N`.
Finishing or skipping writes `$XDG_CONFIG_HOME/drawer/welcomed`.

## Do's and Don'ts

### Do:
- **Do** put live yellow only on keys and selections that act right now; drop a key to faint when its action can't run.
- **Do** carry each machine's hashed tab hue to every place that machine appears.
- **Do** mark focus with the focus-rule heavy top edge plus a default-foreground bold title, and nothing else.
- **Do** set metadata in faint italic and right-align every size and date in a fixed-width column.
- **Do** leave every ground as the terminal background so the active theme shows through; separate panes with hairlines, never fills.
- **Do** strip control characters from any remote-supplied string before drawing it.
- **Do** keep pane titles one word, uppercase and letter-spaced.

### Don't:
- **Don't** use grey panes with a single neon accent or a function-key bar; that is the category default this world replaces.
- **Don't** use stamp magenta for anything but checkout state (stamp, progress, transfer readout).
- **Don't** put faint (ansi 8) text on a selected row (ansi 8 fill); use secondary (ansi 7).
- **Don't** add easing, fades or spinners that animate between states.
- **Don't** use rounded box-drawing corners (`╭╮`); every corner is square.
- **Don't** assign machine hues by list order; hash the alias.
