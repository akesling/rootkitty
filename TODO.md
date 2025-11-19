* Provide some facility for listing against the current directory that's
  referenced by the scan note and seeing a diff now vs. when the scan occurred
* Switch to a gather and then measure approach to provide a better UX (and
  potentially better batching?)
* Allow resumption of paused/cancelled scan for _only_ a given subtree of the
  scan so far
* Show visualizations of space usage: tree view being most important, but also
  progressive pie chart might be valuable?
* It would be cool to be able to have multiple scans going at the same time /
  have an interface where you can see what "background" scans are happening
* Add support for scanning remote machines over SSH
* Display a throbber during scan
* Fix "active directories" for hybrid scan
* Scans view should be made into a "tree" with scans at the leaves.  If two
  scans are for the same root path, they should be siblings under a parent of
  the path.  If a scan includes a sub-tree which has a particular scan
  associated, that should be indicated clearly.
* We seem to only be using 4 cores at about half capacity when we _should_ be
  able to saturate all cores on the machine (should be a perfectly parallel
  task).
* Move this task list to Chox + chox-tui
* Shortcuts at the bottom of the screen extend off the right side / are cut off.
* Shortcuts page should be scrollable
