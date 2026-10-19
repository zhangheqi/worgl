/** Where the site links outside itself. Every figure on the site links to the README section that measured it. */
export const REPO = 'https://github.com/zhangheqi/worgl';
export const README = {
  efficiency: `${REPO}#efficiency`,
  cluster: `${REPO}#running-a-cluster`,
  privacy: `${REPO}#privacy-precisely`,
};
export const DISCUSSIONS = `${REPO}/discussions`;
export const TS_SDK = `${REPO}/tree/main/sdk/typescript`;
export const CRATES = `${REPO}/tree/main/crates`;

const BASE = import.meta.env.BASE_URL.replace(/\/+$/, '');

/** A path on this site, under the base path the site is served from. */
export function page(path = ''): string {
  const p = path.replace(/^\/+/, '');
  return `${BASE}/${p}`;
}
