# boards/example-toolhead

Synthetic reference CAN toolhead board used by `examples/multi-mcu-toolhead`.
Not a real product. Its runtime transport is CAN (`transports.can`) but its
flash method is USB DFU with a bootloader identity distinct from
`boards/example-mainboard` — this matches real CAN toolhead boards, where
flashing happens over a local USB connection independent of the runtime bus.
