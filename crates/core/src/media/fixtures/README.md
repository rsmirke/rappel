# Audio test fixtures

Real audio files for `media/structure/tests.rs` (ADR 0056), each a fraction of a second of GStreamer's test tone, made with GStreamer 1.28 (Ubuntu 26.04):

```sh
gst-launch-1.0 -q audiotestsrc num-buffers=5 samplesperbuffer=1024 ! audio/x-raw,rate=44100,channels=1 ! audioconvert ! lamemp3enc target=bitrate bitrate=32 ! filesink location=tone.mp3
gst-launch-1.0 -q audiotestsrc num-buffers=3 samplesperbuffer=1152 ! audio/x-raw,rate=44100,channels=1 ! audioconvert ! lamemp3enc target=bitrate bitrate=32 ! taginject tags="title=Tone" ! id3v2mux ! filesink location=tone-id3.mp3
gst-launch-1.0 -q audiotestsrc num-buffers=5 samplesperbuffer=960 ! audio/x-raw,rate=48000,channels=1 ! audioconvert ! opusenc bitrate=16000 ! oggmux ! filesink location=tone-opus.ogg
gst-launch-1.0 -q audiotestsrc num-buffers=5 samplesperbuffer=1024 ! audio/x-raw,rate=22050,channels=1 ! audioconvert ! vorbisenc quality=0 ! oggmux ! filesink location=tone-vorbis.ogg
gst-launch-1.0 -q audiotestsrc num-buffers=5 samplesperbuffer=1024 ! audio/x-raw,rate=44100,channels=1 ! audioconvert ! fdkaacenc bitrate=16000 ! mp4mux ! filesink location=tone.m4a
gst-launch-1.0 -q audiotestsrc num-buffers=5 samplesperbuffer=1024 ! audio/x-raw,rate=22050,channels=1,format=S16LE ! wavenc ! filesink location=tone.wav
```

`mp4mux` writes the major brand `mp42`, which Rappel does not take for audio. The M4A's major brand (bytes 8 to 11) was then set to `M4A `, as Apple's tools write it; nothing else was changed:

```sh
python3 -c "d=bytearray(open('tone.m4a','rb').read()); assert d[8:12]==b'mp42'; d[8:12]=b'M4A '; open('tone.m4a','wb').write(d)"
```
