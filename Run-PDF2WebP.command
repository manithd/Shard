#!/bin/bash
# Run-PDF2WebP.command
# Double-click this file to open the PDF to WebP converter in Terminal.
cd "$(dirname "$0")"
./pdf2webp
echo ""
echo "Press Enter to close this window."
read
