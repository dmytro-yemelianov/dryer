# boards/example-mainboard

Synthetic reference mainboard used by Phase 0/1 fixtures and resolver tests.
Not a real product; connector layout loosely follows common 32-bit printer boards.

`1.1.0` adds `downlinks.can0`, the CAN bus this board offers to a child
controller (see `boards/example-toolhead`). `1.0.0` is unchanged and remains
the version every existing example pins.
