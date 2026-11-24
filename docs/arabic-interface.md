# Arabic interface

Arabic is the default when the locale (`LANG`/`LC_*`) starts with `ar`, or with `--lang ar`; English with `--lang en`.
All strings go through `tr!("عربي", "english", args)`. Latin text inside Arabic sentences (paths, codec names) is wrapped in
Unicode bidi isolates so it does not scramble. The panel is `dir="rtl"`, dark by default with a theme toggle, responsive to phone width.
Limit: classic Windows consoles render Arabic poorly; use the panel there.
