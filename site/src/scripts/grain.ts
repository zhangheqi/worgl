/* A fine print grain, drawn once and laid over the page by body::after. */
try {
  const g = document.createElement('canvas');
  g.width = g.height = 180;
  const gx = g.getContext('2d');
  if (gx) {
    const img = gx.createImageData(180, 180), d = img.data;
    for (let i = 0; i < d.length; i += 4) {
      const dark = Math.random() < .5, v = dark ? 17 : 255;
      d[i] = d[i + 1] = d[i + 2] = v;
      d[i + 3] = Math.random() < .55 ? 0 : dark ? 26 : 20;
    }
    gx.putImageData(img, 0, 0);
    document.documentElement.style.setProperty('--grain', `url(${g.toDataURL()})`);
  }
} catch {
  // The page reads fine without grain.
}
