# Testing RustClick on Windows

This package is a **test build**. Native support for Logitech remotes is
experimental: it has been built and unit-tested, but not yet verified on
real hardware. The two programs below collect what is needed to finish it.

| File | What it is |
|---|---|
| `rustclick-diag.exe` | Console tool that records your remotes, their buttons and your target windows into a text report |
| `rustclick.exe` | The application |

Both are unsigned, so Windows SmartScreen may warn: *More info → Run anyway*.
Do not run them as administrator (unless PowerPoint itself runs elevated).

## 1. Diagnostics (please do this first)

1. Plug in the remote's USB receiver or pair it over Bluetooth. For the
   cleanest result, quit Logitech Presentation / Logi Options+ if they run;
   if you normally use them, run the tool once with and once without.
2. Double-click `rustclick-diag.exe`. A console opens and the report file
   `rustclick-diag-<date>-<time>.txt` is created next to the exe (or in
   `%TEMP%` if that folder is read-only); its path is printed at the top and
   the bottom.
3. Follow the prompts in the console:
   * **HID++ probe**: press a button on each remote to wake it up, then
     press Enter.
   * **Capture 1**: press every button of every remote, one at a time:
     short, long (hold 2 s) and double press. Then press Enter.
   * **Diversion test** (asked only if the remote supports it): answer `1`.
     Press every button again, then Enter. The tool restores the buttons
     afterwards; if a button ever stays silent, switch the remote off and on.
   * **Delivery test**: open a presentation in PowerPoint (ideally start the
     slide show) or a PDF, answer `1`, type the number of that window from
     the list, and keep the console in the foreground. After each attempt,
     answer whether the slide moved forward (`1`) or not (`0`).
4. Send the report file.

The report contains device names and ids, window titles of open windows and
which keys you pressed in the console while the capture ran. Nothing is sent
anywhere by the tool itself.

## 2. The application

1. Start `rustclick.exe`. It opens in simulation mode.
2. *Settings* → *Input mode* → **Native**.
3. Your Logitech remote appears in the device list, **disabled**. Enable it.
4. Open the presentation in PowerPoint, then choose it as the target
   (*Change…*; it is selected automatically if it is the only one).
5. *Start* routing, put another window in the foreground (e.g. Notepad), and
   press the remote's buttons.
6. Note what happened in PowerPoint and in the foreground window.
   **Known limitation:** the original keystroke still reaches the foreground
   window as well; suppressing it is the next step.
7. Send the log file: *Settings* shows its location
   (`%LOCALAPPDATA%\RustClick\data\rustclick.log`). It records debug details
   regardless of the log level set in the application.

## What the results decide

* Which keys or HID++ controls each remote and button produce, and whether
  long presses can be told apart.
* Whether HID++ button diversion works for R500s and Spotlight, which would
  suppress the original keystrokes without hooks or drivers.
* Which window and method actually make PowerPoint or Acrobat react while
  they are in the background.
