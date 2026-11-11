#!/usr/bin/env sh
# Downloads the bundled text fonts (SIL OFL / Apache 2.0) from Google Fonts into assets/fonts.
# Each line: file stem | Google Fonts family | regular weight | bold weight (or -) | license path in google/fonts
set -e
cd "$(dirname "$0")/../assets/fonts"
while IFS='|' read -r stem family reg bold license; do
  [ -z "$stem" ] && continue
  q=$(printf %s "$family" | tr ' ' '+')
  weights=$reg; [ "$bold" != "-" ] && weights="$reg;$bold"
  css=$(curl -fsS "https://fonts.googleapis.com/css2?family=$q:wght@$weights")
  for w in $reg $bold; do
    [ "$w" = "-" ] && continue
    url=$(printf '%s\n' "$css" | awk -v w="$w" '/font-weight:/{gsub(/[^0-9]/,"",$2); cur=$2} /src: url/{ if (cur==w) { match($0, /https:[^)]*/); print substr($0, RSTART, RLENGTH) } }')
    suffix=""; [ "$w" = "$bold" ] && suffix="-bold"
    curl -fsS -o "$stem$suffix.ttf" "$url"
  done
  curl -fsS -o "$stem-LICENSE.txt" "https://raw.githubusercontent.com/google/fonts/main/$license"
done <<'LIST'
great-vibes|Great Vibes|400|-|ofl/greatvibes/OFL.txt
pacifico|Pacifico|400|-|ofl/pacifico/OFL.txt
dancing-script|Dancing Script|500|700|ofl/dancingscript/OFL.txt
lobster|Lobster|400|-|ofl/lobster/OFL.txt
satisfy|Satisfy|400|-|apache/satisfy/LICENSE.txt
sacramento|Sacramento|400|-|ofl/sacramento/OFL.txt
caveat|Caveat|500|700|ofl/caveat/OFL.txt
bebas-neue|Bebas Neue|400|-|ofl/bebasneue/OFL.txt
anton|Anton|400|-|ofl/anton/OFL.txt
montserrat|Montserrat|500|800|ofl/montserrat/OFL.txt
poppins|Poppins|500|800|ofl/poppins/OFL.txt
oswald|Oswald|500|700|ofl/oswald/OFL.txt
playfair-display|Playfair Display|500|800|ofl/playfairdisplay/OFL.txt
permanent-marker|Permanent Marker|400|-|apache/permanentmarker/LICENSE.txt
bangers|Bangers|400|-|ofl/bangers/OFL.txt
LIST
