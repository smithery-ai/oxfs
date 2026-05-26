#!/usr/bin/env node
// Generate oxfs illustration SVG from parameters.
// Usage: node docs/gen-illustration.js > docs/illustration.svg

// ─── Tuning knobs ───────────────────────────────────────────
const TILE    = 100   // tile width & height
const GAP     = 30    // gap between right-grid tiles
const ROW_GAP = 130   // vertical distance between row centers
const MARGIN  = 80    // canvas margin
const SPINE_GAP = 80  // gap between tile edge and spine
const CENTER_W = 160  // oxfs tile width
const CENTER_H = 100  // oxfs tile height
const R       = 0     // tile corner radius
const ICON_S  = TILE / 24 * 0.58
const ICON_PAD = (TILE - 24 * ICON_S) / 2
const BG      = '#1a1a1a'
const GRID_N  = 4     // right grid is GRID_N x GRID_N
const LEFT_N  = 2     // left column tile count
const LEFT_GAP = 30   // gap between left tiles

// ─── Derived layout ─────────────────────────────────────────
// Right grid: 4 rows
const rightRowY = Array.from({length: GRID_N}, (_, i) => MARGIN + TILE / 2 + i * ROW_GAP)

// Right spine: vertical midpoint of the grid
const rightSpineMidY = (rightRowY[0] + rightRowY[GRID_N - 1]) / 2

// Left: 2 tiles (Docker + Firecracker) stacked, centered on midpoint
const leftX = MARGIN
const leftTotalH = LEFT_N * TILE + (LEFT_N - 1) * LEFT_GAP
const leftTopY = rightSpineMidY - leftTotalH / 2
const leftTiles = Array.from({length: LEFT_N}, (_, i) => ({
  x: leftX,
  y: leftTopY + i * (TILE + LEFT_GAP),
}))
const leftRowY = leftTiles.map(t => t.y + TILE / 2)
const leftSpineX = leftX + TILE + SPINE_GAP

// oxfs: centered vertically on rightSpineMidY
const oxfsGap = 80
const centerX = leftSpineX + oxfsGap
const centerY = rightSpineMidY
const centerTile = { x: centerX, y: centerY - CENTER_H / 2, w: CENTER_W, h: CENTER_H }

// Right spine: to the right of oxfs, with gap, and to the left of right grid, with gap
const rightSpineX = centerX + CENTER_W + SPINE_GAP

// Right grid columns: start after spine + gap
const rightGridX = rightSpineX + SPINE_GAP
const rightCols = Array.from({length: GRID_N}, (_, c) => rightGridX + c * (TILE + GAP))
const rightTiles = rightRowY.flatMap((cy, r) =>
  rightCols.map((cx, c) => ({ x: cx, y: cy - TILE / 2, row: r, col: c }))
)

// Canvas
const canvasW = rightCols[GRID_N - 1] + TILE + MARGIN
const canvasH = rightRowY[GRID_N - 1] + TILE / 2 + MARGIN

// ─── Icon data (simple-icons 24x24 viewBox) ─────────────────
const icons = {
  oci: { color: '#999999', path: 'M0 0v24h24V0zm20.547 20.431H3.448V3.573h17.104V20.43zm-5.155-9.979h3.436v8.255h-3.436zm0-5.16h3.436v3.436h-3.436zm-6.789 9.976V8.732h5.074v-3.44H5.164v13.415h8.513v-3.44Z' },
  s3: { color: '#569A31', path: 'M20.913 13.147l.12-.895c.947.576 1.258.922 1.354 1.071-.16.031-.562.046-1.474-.176zm-2.174 7.988a.547.547 0 0 0-.005.073c0 .084-.207.405-1.124.768a10.28 10.28 0 0 1-1.438.432c-1.405.325-3.128.504-4.853.504-4.612 0-7.412-1.184-7.412-1.704a.547.547 0 0 0-.005-.073L1.81 5.602c.135.078.28.154.432.227.042.02.086.038.128.057.134.062.272.122.417.18l.179.069c.154.058.314.114.478.168.043.013.084.029.13.043.207.065.423.127.646.187l.176.044c.175.044.353.087.534.127a23.414 23.414 0 0 0 .843.17l.121.023c.252.045.508.085.768.122.071.011.144.02.216.03.2.027.4.053.604.077l.24.027c.245.026.49.05.74.07l.081.009c.275.022.552.04.83.056l.233.012c.21.01.422.018.633.025a33.088 33.088 0 0 0 2.795-.026l.232-.011c.278-.016.555-.034.83-.056l.08-.008c.25-.02.497-.045.742-.072l.238-.026c.205-.024.408-.05.609-.077.07-.01.141-.019.211-.03.261-.037.519-.078.772-.122l.111-.02c.215-.04.427-.082.634-.125l.212-.047c.186-.041.368-.085.546-.13l.166-.042c.225-.06.444-.122.654-.189.04-.012.077-.026.115-.038a10.6 10.6 0 0 0 .493-.173c.058-.021.114-.044.17-.066.15-.06.293-.12.43-.185.038-.017.079-.034.116-.052.153-.073.3-.15.436-.228l-.976 7.245c-2.488-.78-5.805-2.292-7.311-3a1.09 1.09 0 0 0-1.088-1.085c-.6 0-1.088.489-1.088 1.088 0 .6.488 1.089 1.088 1.089.196 0 .378-.056.537-.148 1.72.812 5.144 2.367 7.715 3.15zm-7.42-20.047c5.677 0 9.676 1.759 9.75 2.736l-.014.113c-.01.033-.031.067-.048.101-.015.028-.026.057-.047.087-.024.033-.058.068-.09.102-.028.03-.051.06-.084.09-.038.035-.087.07-.133.105-.04.03-.074.06-.119.091-.053.036-.116.071-.177.107-.05.03-.095.06-.15.09-.068.036-.147.073-.222.11-.059.028-.114.057-.177.085-.084.038-.177.074-.268.111-.068.027-.13.054-.203.082-.097.036-.205.072-.31.107-.075.026-.148.053-.228.079-.111.035-.233.069-.35.103-.085.024-.165.05-.253.073-.124.034-.258.065-.389.098-.093.022-.181.046-.278.068-.139.032-.287.061-.433.091-.098.02-.191.041-.293.06-.155.03-.32.057-.482.084-.1.018-.198.036-.302.052-.166.026-.342.048-.515.072-.11.014-.213.03-.325.044-.181.023-.372.041-.56.06-.11.012-.218.025-.332.036-.188.016-.386.029-.58.043-.122.009-.24.02-.364.028-.207.012-.422.02-.635.028-.12.005-.234.012-.354.016a35.605 35.605 0 0 1-2.069 0c-.12-.004-.234-.011-.352-.016-.214-.008-.43-.016-.637-.028-.122-.008-.238-.02-.36-.027-.195-.015-.394-.028-.584-.044-.11-.01-.215-.024-.324-.035-.19-.02-.384-.038-.568-.06l-.315-.044c-.176-.024-.355-.046-.525-.073-.1-.015-.192-.033-.29-.05-.167-.028-.335-.055-.494-.086-.096-.018-.183-.038-.276-.056-.151-.032-.305-.062-.45-.095-.09-.02-.173-.043-.26-.064-.138-.034-.277-.067-.407-.102-.082-.022-.157-.046-.235-.069a11.75 11.75 0 0 1-.368-.108c-.075-.024-.141-.049-.213-.073-.11-.037-.223-.075-.325-.113-.067-.025-.125-.051-.188-.077-.096-.038-.195-.076-.282-.115-.06-.027-.11-.054-.166-.08-.08-.039-.162-.077-.233-.116-.052-.028-.094-.055-.142-.084-.063-.038-.13-.075-.185-.113-.043-.029-.075-.058-.113-.086-.048-.037-.098-.073-.139-.11-.032-.029-.054-.057-.08-.087-.033-.035-.069-.07-.093-.104-.02-.03-.031-.058-.046-.086-.018-.035-.039-.068-.049-.102l-.015-.113c.076-.977 4.074-2.736 9.748-2.736zm12.182 12.124c-.118-.628-.84-1.291-2.31-2.128l.963-7.16a.531.531 0 0 0 .005-.073C22.16 1.581 16.447 0 11.32 0 6.194 0 .482 1.581.482 3.851a.58.58 0 0 0 .005.072L2.819 21.25c.071 2.002 5.236 2.75 8.5 2.75 1.805 0 3.615-.188 5.098-.531.598-.138 1.133-.3 1.592-.48 1.18-.467 1.789-1.053 1.813-1.739l.945-7.018c.557.131 1.016.197 1.389.197.54 0 .902-.137 1.134-.413a.956.956 0 0 0 .21-.804Z' },
  cloudflare: { color: '#F38020', path: 'M16.5088 16.8447c.1475-.5068.0908-.9707-.1553-1.3154-.2246-.3164-.6045-.499-1.0615-.5205l-8.6592-.1123a.1559.1559 0 0 1-.1333-.0713c-.0283-.042-.0351-.0986-.021-.1553.0278-.084.1123-.1484.2036-.1562l8.7359-.1123c1.0351-.0489 2.1601-.8868 2.5537-1.9136l.499-1.3013c.0215-.0561.0293-.1128.0147-.168-.5625-2.5463-2.835-4.4453-5.5499-4.4453-2.5039 0-4.6284 1.6177-5.3876 3.8614-.4927-.3658-1.1187-.5625-1.794-.499-1.2026.119-2.1665 1.083-2.2861 2.2856-.0283.31-.0069.6128.0635.894C1.5683 13.171 0 14.7754 0 16.752c0 .1748.0142.3515.0352.5273.0141.083.0844.1475.1689.1475h15.9814c.0909 0 .1758-.0645.2032-.1553l.12-.4268zm2.7568-5.5634c-.0771 0-.1611 0-.2383.0112-.0566 0-.1054.0415-.127.0976l-.3378 1.1744c-.1475.5068-.0918.9707.1543 1.3164.2256.3164.6055.498 1.0625.5195l1.8437.1133c.0557 0 .1055.0263.1329.0703.0283.043.0351.1074.0214.1562-.0283.084-.1132.1485-.204.1553l-1.921.1123c-1.041.0488-2.1582.8867-2.5527 1.914l-.1406.3585c-.0283.0713.0215.1416.0986.1416h6.5977c.0771 0 .1474-.0489.169-.126.1122-.4082.1757-.837.1757-1.2803 0-2.6025-2.125-4.727-4.7344-4.727' },
  gcs: { color: '#4285F4', path: 'M12.19 2.38a9.344 9.344 0 0 0-9.234 6.893c.053-.02-.055.013 0 0-3.875 2.551-3.922 8.11-.247 10.941l.006-.007-.007.03a6.717 6.717 0 0 0 4.077 1.356h5.173l.03.03h5.192c6.687.053 9.376-8.605 3.835-12.35a9.365 9.365 0 0 0-2.821-4.552l-.043.043.006-.05A9.344 9.344 0 0 0 12.19 2.38zm-.358 4.146c1.244-.04 2.518.368 3.486 1.15a5.186 5.186 0 0 1 1.862 4.078v.518c3.53-.07 3.53 5.262 0 5.193h-5.193l-.008.009v-.04H6.785a2.59 2.59 0 0 1-1.067-.23h.001a2.597 2.597 0 1 1 3.437-3.437l3.013-3.012A6.747 6.747 0 0 0 8.11 8.24c.018-.01.04-.026.054-.023a5.186 5.186 0 0 1 3.67-1.69z' },
  gdrive: { color: '#4285F4', path: 'M12.01 1.485c-2.082 0-3.754.02-3.743.047.01.02 1.708 3.001 3.774 6.62l3.76 6.574h3.76c2.081 0 3.753-.02 3.742-.047-.005-.02-1.708-3.001-3.775-6.62l-3.76-6.574zm-4.76 1.73a789.828 789.861 0 0 0-3.63 6.319L0 15.868l1.89 3.298 1.885 3.297 3.62-6.335 3.618-6.33-1.88-3.287C8.1 4.704 7.255 3.22 7.25 3.214zm2.259 12.653-.203.348c-.114.198-.96 1.672-1.88 3.287a423.93 423.948 0 0 1-1.698 2.97c-.01.026 3.24.042 7.222.042h7.244l1.796-3.157c.992-1.734 1.85-3.23 1.906-3.323l.104-.167h-7.249z' },
  dropbox: { color: '#0061FF', path: 'M6 1.807L0 5.629l6 3.822 6.001-3.822L6 1.807zM18 1.807l-6 3.822 6 3.822 6-3.822-6-3.822zM0 13.274l6 3.822 6.001-3.822L6 9.452l-6 3.822zM18 9.452l-6 3.822 6 3.822 6-3.822-6-3.822zM6 18.371l6.001 3.822 6-3.822-6-3.822L6 18.371z' },
  minio: { color: '#C72C48', path: 'M13.2072.006c-.6216-.0478-1.2.1943-1.6211.582a2.15 2.15 0 0 0-.0938 3.0352l3.4082 3.5507a3.042 3.042 0 0 1-.664 4.6875l-.463.2383V7.2853a15.4198 15.4198 0 0 0-8.0174 10.4862v.0176l6.5487-3.3281v7.621L13.7794 24V13.6817l.8965-.4629a4.4432 4.4432 0 0 0 1.2207-7.0292l-3.371-3.5254a.7489.7489 0 0 1 .037-1.0547.7522.7522 0 0 1 1.0567.0371l.4668.4863-.006.0059 4.0704 4.2441a.0566.0566 0 0 0 .082 0 .06.06 0 0 0 0-.0703l-3.1406-5.1425-.1484.1425.1484-.1445C14.4945.3926 13.8287.0538 13.2072.006Zm-.9024 9.8652v2.9941l-4.1523 2.1484a13.9787 13.9787 0 0 1 2.7676-3.9277 14.1784 14.1784 0 0 1 1.3847-1.2148z' },
  azure: { color: '#0078D4', path: 'M22.379 23.343a1.62 1.62 0 0 0 1.536-2.14v.002L17.35 1.76A1.62 1.62 0 0 0 15.816.657H8.184A1.62 1.62 0 0 0 6.65 1.76L.086 21.204a1.62 1.62 0 0 0 1.536 2.139h4.741a1.62 1.62 0 0 0 1.535-1.103l.977-2.892 4.947 3.675c.28.208.618.32.966.32m-3.084-12.531 3.624 10.739a.54.54 0 0 1-.51.713v-.001h-.03a.54.54 0 0 1-.322-.106l-9.287-6.9h4.853m6.313 7.006c.116-.326.13-.694.007-1.058L9.79 1.76a1.722 1.722 0 0 0-.007-.02h6.034a.54.54 0 0 1 .512.366l6.562 19.445a.54.54 0 0 1-.338.684' },
  digitalocean: { color: '#0080FF', path: 'M12.04 0C5.408-.02.005 5.37.005 11.992h4.638c0-4.923 4.882-8.731 10.064-6.855a6.95 6.95 0 014.147 4.148c1.889 5.177-1.924 10.055-6.84 10.064v-4.61H7.391v4.623h4.61V24c7.86 0 13.967-7.588 11.397-15.83-1.115-3.59-3.985-6.446-7.575-7.575A12.8 12.8 0 0012.039 0zM7.39 19.362H3.828v3.564H7.39zm-3.563 0v-2.978H.85v2.978z' },
  hetzner: { color: '#D50C2D', path: 'M0 0v24h24V0H0zm4.602 4.025h2.244c.509 0 .716.215.716.717v5.64h8.883v-5.64c0-.509.215-.717.717-.717h2.229c.5 0 .71.23.724.717v14.516c0 .509-.215.717-.717.717h-2.23c-.51 0-.717-.215-.717-.717v-5.735H7.562v5.735c0 .516-.215.717-.716.717H4.602c-.51 0-.717-.208-.717-.717V4.742c0-.509.207-.717.717-.717z' },
  oracle: { color: '#F80000', path: 'M16.412 4.412h-8.82a7.588 7.588 0 0 0-.008 15.176h8.828a7.588 7.588 0 0 0 0-15.176zm-.193 12.502H7.786a4.915 4.915 0 0 1 0-9.828h8.433a4.914 4.914 0 1 1 0 9.828z' },
  wasabi: { color: '#56B946', path: 'M20.483 3.517A11.91 11.91 0 0 0 12 0a11.91 11.91 0 0 0-8.483 3.517A11.91 11.91 0 0 0 0 12a11.91 11.91 0 0 0 3.517 8.483A11.91 11.91 0 0 0 12 24a11.91 11.91 0 0 0 8.483-3.517A11.91 11.91 0 0 0 24 12a11.91 11.91 0 0 0-3.517-8.483Zm1.29 7.387-5.16-4.683-5.285 4.984-2.774 2.615V9.932l4.206-3.994 3.146-2.969c3.163 1.379 5.478 4.365 5.867 7.935zm-.088 2.828a10.632 10.632 0 0 1-1.025 2.951l-2.952-2.668v-3.87Zm-8.183-11.47-2.227 2.103-2.739 2.598v-4.17A9.798 9.798 0 0 1 12 2.155c.513 0 1.007.035 1.502.106zM6.398 13.891l-4.083-3.658a9.744 9.744 0 0 1 1.078-2.987L6.398 9.95zm0-9.968v3.129l-1.75-1.573a8.623 8.623 0 0 1 1.75-1.556Zm-4.189 9.102 5.284 4.736 5.302-4.983 2.74-2.598v3.817l-7.423 7.016a9.823 9.823 0 0 1-5.903-7.988Zm8.306 8.695 5.02-4.754v4.206a9.833 9.833 0 0 1-3.553.654c-.495 0-.99-.035-1.467-.106zm7.176-1.714v-3.11l1.714 1.555a9.604 9.604 0 0 1-1.714 1.555z' },
  alibaba: { color: '#FF6A00', path: 'M3.996 4.517h5.291L8.01 6.324 4.153 7.506a1.668 1.668 0 0 0-1.165 1.601v5.786a1.668 1.668 0 0 0 1.165 1.6l3.857 1.183 1.277 1.807H3.996A3.996 3.996 0 0 1 0 15.487V8.513a3.996 3.996 0 0 1 3.996-3.996m16.008 0h-5.291l1.277 1.807 3.857 1.182c.715.227 1.17.889 1.165 1.601v5.786a1.668 1.668 0 0 1-1.165 1.6l-3.857 1.183-1.277 1.807h5.291A3.996 3.996 0 0 0 24 15.487V8.513a3.996 3.996 0 0 0-3.996-3.996m-4.007 8.345H8.002v-1.804h7.995Z' },
}

// ─── Tile definitions ────────────────────────────────────────
const leftDefs = [
  { icon: 'oci' },
  { png: 'fc-icon-sm.png' },
]

const rightDefs = [
  { icon: 's3' }, { icon: 'cloudflare' }, { icon: 'gcs' }, { icon: 'azure' },
  { icon: 'gdrive' }, { icon: 'dropbox' }, { icon: 'minio' }, { icon: 'digitalocean' },
  { icon: 'hetzner' }, { icon: 'oracle' }, { icon: 'wasabi' }, { icon: 'alibaba' },
  { text: 'T', textColor: '#00C2FF' }, { text: 'B2', textColor: '#E21E29' },
  { text: 'W', textColor: '#7B68EE' }, { text: '50+', textColor: '#4a4a6a', opacity: 0.5 },
]

// ─── SVG builders ────────────────────────────────────────────
function tile(x, y, w, h, cls, opacity) {
  const op = opacity != null ? ` opacity="${opacity}"` : ''
  return `  <rect class="${cls}" x="${x}" y="${y}" width="${w}" height="${h}" rx="${R}"${op}/>`
}

function iconG(x, y, iconKey) {
  const ic = icons[iconKey]
  return `  <g transform="translate(${x + ICON_PAD},${y + ICON_PAD}) scale(${ICON_S.toFixed(3)})">
    <path fill="${ic.color}" d="${ic.path}"/>
  </g>`
}

function textTile(x, y, text, color) {
  const fs = text.length > 2 ? 26 : 48
  return `  <text style="fill:${color};font-family:-apple-system,sans-serif;font-size:${fs}px;font-weight:800;text-anchor:middle;dominant-baseline:central" x="${x + TILE / 2}" y="${y + TILE / 2}">${text}</text>`
}

function ln(x1, y1, x2, y2) {
  return `  <line class="conn" x1="${x1}" y1="${y1}" x2="${x2}" y2="${y2}"/>`
}

// ─── Assemble SVG ────────────────────────────────────────────
const out = []
const push = s => out.push(s)

push(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${canvasW} ${canvasH}" fill="none">`)
push(`  <defs>`)
push(`    <style>`)
push(`      .tile{fill:#252525;stroke:#3a3a3a;stroke-width:1}`)
push(`      .tile-center{fill:#252525;stroke:#f97316;stroke-width:2.5}`)
push(`      .conn{stroke:#3a3a3a;stroke-width:1.5;fill:none}`)
push(`    </style>`)
// Gradient fades
const fadeW = 200
push(`    <linearGradient id="ft" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${BG}"/><stop offset="1" stop-color="${BG}" stop-opacity="0"/></linearGradient>`)
push(`    <linearGradient id="fb" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="${BG}"/><stop offset="1" stop-color="${BG}" stop-opacity="0"/></linearGradient>`)
push(`    <linearGradient id="fr" x1="1" y1="0" x2="0" y2="0"><stop offset="0" stop-color="${BG}"/><stop offset="1" stop-color="${BG}" stop-opacity="0"/></linearGradient>`)
push(`  </defs>`)
push(`  <rect width="${canvasW}" height="${canvasH}" fill="${BG}"/>`)

// ─── Connectors ──────────────────────────────────────────────
push(``)
push(`  <!-- Connectors -->`)

// Left vertical spine
push(ln(leftSpineX, leftRowY[0], leftSpineX, leftRowY[LEFT_N - 1]))
// Left horizontal stubs: tile right edge to spine
for (let i = 0; i < LEFT_N; i++) {
  push(ln(leftX + TILE, leftRowY[i], leftSpineX, leftRowY[i]))
}
// Spine to oxfs
push(ln(leftSpineX, centerY, centerX, centerY))

// E: right vertical spine
push(ln(rightSpineX, rightRowY[0], rightSpineX, rightRowY[GRID_N - 1]))
// oxfs to right spine
push(ln(centerX + CENTER_W, centerY, rightSpineX, centerY))
// Row rails: spine to first tile left edge, then through all tiles
const fullRight = rightCols[GRID_N - 1] + TILE
for (let r = 0; r < GRID_N; r++) {
  push(ln(rightSpineX, rightRowY[r], rightCols[0], rightRowY[r]))
  push(ln(rightCols[0], rightRowY[r], fullRight, rightRowY[r]))
}

// ─── Left tiles: compute environments ────────────────────────
push(``)
push(`  <!-- Left: Compute -->`)
leftTiles.forEach((t, i) => {
  const def = leftDefs[i]
  push(tile(t.x, t.y, TILE, TILE, 'tile'))
  if (def.icon) {
    push(iconG(t.x, t.y, def.icon))
  } else if (def.png) {
    const imgPad = TILE * 0.15
    const imgSize = TILE - imgPad * 2
    push(`  <image href="${def.png}" x="${t.x + imgPad}" y="${t.y + imgPad}" width="${imgSize}" height="${imgSize}"/>`)
  }
})

// ─── Center tile ─────────────────────────────────────────────
push(``)
push(`  <!-- Center: oxfs -->`)
push(tile(centerTile.x, centerTile.y, centerTile.w, centerTile.h, 'tile-center'))
push(`  <text style="fill:#f97316;font-family:-apple-system,sans-serif;font-size:32px;font-weight:700;text-anchor:middle" x="${centerTile.x + centerTile.w / 2}" y="${centerTile.y + centerTile.h / 2 + 10}">oxfs</text>`)

// ─── Right tiles ─────────────────────────────────────────────
push(``)
push(`  <!-- Right: Storage backends -->`)
rightTiles.forEach((t, i) => {
  const def = rightDefs[i]
  push(tile(t.x, t.y, TILE, TILE, 'tile', def.opacity))
  if (def.icon) {
    push(iconG(t.x, t.y, def.icon))
  } else {
    push(textTile(t.x, t.y, def.text, def.textColor))
  }
})

// ─── Edge fades ──────────────────────────────────────────────
push(``)
push(`  <!-- Edge fades -->`)
const gridTop = rightRowY[0] - TILE / 2
const gridBot = rightRowY[GRID_N - 1] + TILE / 2
const gridLeft = rightCols[0]
const gridRight = rightCols[GRID_N - 1] + TILE
push(`  <rect x="${gridLeft - GAP}" y="${gridTop - 10}" width="${gridRight - gridLeft + GAP + MARGIN}" height="${fadeW}" fill="url(#ft)"/>`)
push(`  <rect x="${gridLeft - GAP}" y="${gridBot - fadeW + 10}" width="${gridRight - gridLeft + GAP + MARGIN}" height="${fadeW}" fill="url(#fb)"/>`)
push(`  <rect x="${gridRight - fadeW + 10}" y="${gridTop - 10}" width="${fadeW + MARGIN}" height="${gridBot - gridTop + 20}" fill="url(#fr)"/>`)

push(`</svg>`)

process.stdout.write(out.join('\n') + '\n')
