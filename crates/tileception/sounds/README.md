# Notification sounds

The sounds Tileception plays in the Sound mode, built into the binary by
`sound.rs`. There are three families (Vibes, Knock and Breath), each with a
sound for low, normal and critical notifications and a looping alarm for the
built-in Timers.

Each `.ogg` was rendered from the MIDI file of the same name in `midi/` with
the FluidR3 GM soundfont (MIT licence) through tinysoundfont, then encoded as
Ogg Vorbis. Alarms loop without a gap: their length is a whole number of bars.
