# Benchmark workload

`workload.html` is a fixed 30-second visual workload so scry and the Microsoft Windows App
can be compared on the **same Cloud PC and network**. Phases: idle → text scroll → moving box
→ full-screen flashes. Motion is time-based, so every run is identical regardless of frame rate.
A large HUD shows elapsed time, the page's own fps, a frame counter and a millisecond clock
(readable on a phone slow-mo recording).

## Run it

1. Get `workload.html` onto the Cloud PC (OneDrive/SharePoint, email-to-self, or any approved
   path) and open it in Chrome there. It needs no network or hosting.
2. Press **F** (fullscreen), then **Space** to run the 30 s test. **Space** restarts it.
3. Do the exact same run once connected with **scry** and once with **Windows App**.

## Measure (from the Mac, no camera)

`measure.sh` samples network, CPU and memory for whichever client is connected, over the
workload window, and appends a row to `results.csv` so the two clients line up:

```sh
# connect with scry and run the workload, then in another terminal:
bench/measure.sh scry 35

# reconnect with the Microsoft client and run the same workload:
bench/measure.sh "Windows App" 35
```

Start it as the workload's IDLE phase begins. It reports download/upload MB and kbit/s, average
CPU % (across cores) and peak memory. Keep the **window size the same** for both clients so the
pixel counts match. Compare the two rows in `results.csv`.

scry can also print its own pipeline numbers live with `SCRY_STATS=1 … 2> stats.log`
(`PDUs/s`, `MB/s`, decode %, `viewer … redraws/s`). The Windows App is a black box, so its
frame rate isn't directly measurable without a camera — compare bandwidth, CPU and memory, which
`measure.sh` covers for both.
