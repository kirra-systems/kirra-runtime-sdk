#!/usr/bin/env bash
exec ~/piper/piper --model ~/piper/en_US-lessac-medium.onnx --output-raw \
  | aplay -D plughw:CARD=UACDemoV10,DEV=0 -r 22050 -f S16_LE -t raw -
