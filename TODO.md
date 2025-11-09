* Status should probably be a line that, when clicked, expands if there's more
  text, instead of always being an outlined "pane"
* Provide some facility for listing against the current directory that's
  referenced by the scan note and seeing a diff now vs. when the scan occurred
* Provide the ability to drop into a shell in a given displayed directory or
  parent of a file
* Display how large the scan itself is on disk
* Switch to a gather and then measure approach to provide a better UX (and
  potentially better batching?)
* Switch to using the walkdir crate to efficiently walk the FS
* On settings screen j/k should skip the empty lines
* Allow resumption of paused/cancelled scan for _only_ a given subtree of the
  scan so far
