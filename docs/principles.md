# Principles

1. Protect the user from data loss — misuse can mean a costly full recovery.
2. Protect against incorrect benchmark results — wrong data is as bad as lost data.
3. Slick and foolproof — minimize the opportunity for misuse; make it hard to do the wrong thing.
4. Fail safe — prepare for crashes, never leave the system in a dirty state.
5. Verify before acting — checksum volumes, detect drift.
6. Check the environment — validate tools, versions, and prerequisites are present.
7. Be stateful — configure once, remember everything.
8. Interactive by default — commands prompt for input rather than assuming.
9. Require confirmation for destructive operations — via prompt or `-y`.
10. Clean up after yourself — tear down dm-era devices, don't leak resources.
11. Principle of least surprise — behave the way the user expects; no hidden side effects.
12. Be transparent — every action the program takes should be printed to the screen.
