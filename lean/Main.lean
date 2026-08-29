import PathMapModel

/-!
The differential oracle.

    pathmap-oracle <input-file>       -- decode and run the file's bytes
    pathmap-oracle                    -- read the bytes from stdin
    pathmap-oracle --act <input-file> -- ArenaCompactTree mode: skip the
                                      -- operations an ACT read source cannot
                                      -- serve, matching examples/act_trace.rs

Prints the trace produced by the model.  `examples/pathmap_trace.rs` prints the
same trace from the real crate for the same input bytes, and
`examples/act_trace.rs` does the same with an `ArenaCompactTree` as the read
source; the two are compared by `lean/differential.py`.
-/

def main (args : List String) : IO Unit := do
  let act := args.contains "--act"
  let files := args.filter (· != "--act")
  let bytes ← match files with
    | [] => (← IO.getStdin).readBinToEnd
    | path :: _ => IO.FS.readBinFile path
  for line in PathMapModel.Fuzz.run bytes 256 act do
    IO.println line
