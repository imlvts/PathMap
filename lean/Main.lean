import PathMapModel

/-!
The differential oracle.

    pathmap-oracle <input-file>     -- decode and run the file's bytes
    pathmap-oracle                  -- read the bytes from stdin

Prints the trace produced by the model.  `examples/pathmap_trace.rs` prints the
same trace from the real crate for the same input bytes; the two are compared by
`lean/differential.py`.
-/

def main (args : List String) : IO Unit := do
  let bytes ← match args with
    | [] => (← IO.getStdin).readBinToEnd
    | path :: _ => IO.FS.readBinFile path
  for line in PathMapModel.Fuzz.run bytes do
    IO.println line
