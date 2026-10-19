# Audio of formats Rappel does not play

Test files for `gst_env::tests::only_the_allowed_plugins_play` (ADR 0062): audio in formats whose `GStreamer` plugins are not on Rappel's allow-list, so they must not play even when handed to `gtk::MediaFile` directly. Each is a fraction of a second of `GStreamer`'s test tone, made with `GStreamer` 1.28 (Ubuntu 26.04):

```sh
gst-launch-1.0 -q audiotestsrc num-buffers=5 samplesperbuffer=1024 ! audio/x-raw,rate=22050,channels=1,format=S16LE ! flacenc ! filesink location=tone.flac
gst-launch-1.0 -q audiotestsrc num-buffers=5 samplesperbuffer=1024 ! audio/x-raw,rate=22050,channels=1 ! audioconvert ! vorbisenc quality=0 ! matroskamux ! filesink location=tone-vorbis.mka
```

`tone.flac` needs `flacdec` (plugin `flac`), whose parser (`flacparse`, in the allowed `audioparsers`) is not enough; `tone-vorbis.mka` holds Vorbis, which Rappel decodes, in Matroska, whose demuxer (plugin `matroska`) is not allowed.
