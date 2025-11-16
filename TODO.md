* Provide some facility for listing against the current directory that's
  referenced by the scan note and seeing a diff now vs. when the scan occurred
* Switch to a gather and then measure approach to provide a better UX (and
  potentially better batching?)
* Allow resumption of paused/cancelled scan for _only_ a given subtree of the
  scan so far
* Show visualizations of space usage: tree view being most important, but also
  progressive pie chart might be valuable?
* For garbage collection simplicity, etc. each scan should be its own table and
  there should be a scans table that stores metadata about each scan, including
  the table in which it is stored
* While a scan is loading, we should show some form of throbber in the middle of
  the screen
