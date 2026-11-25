"""Render trained-policy episodes as a self-contained HTML replay.

Plays episodes through territory_server (greedy or uniform), captures
every decision-point board, and writes an SVG animation: provinces
colored by controller, fort/siege marks, army stacks, the chosen order,
and a day/reward HUD. Open the file in a browser; arrow keys step.
"""
import json, subprocess as sp, sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from eu4_decide._territory_raft import TerritoryGNN, parse  # noqa: E402

import torch  # noqa: E402

HTML = """<!DOCTYPE html>
<html lang="zh"><head><meta charset="utf-8"><title>EU4 决策回放</title>
<style>
 body{background:#1a1a24;color:#ddd;font-family:system-ui,sans-serif;margin:0;display:flex;flex-direction:column;align-items:center}
 h2{margin:8px;font-weight:500}
 #hud{font-size:14px;color:#9ab;margin:4px}
 svg{background:#12121a;border-radius:8px}
 .edge{stroke:#3a3a52;stroke-width:1.2}
 .strait{stroke:#4a6a9a;stroke-width:1.2;stroke-dasharray:4 3}
 circle.p{stroke:#000;stroke-width:0.8;cursor:pointer}
 circle.p:hover{stroke:#fff}
 .lbl{fill:#cdd;font-size:8px;text-anchor:middle;pointer-events:none}
 #ctl{margin:10px;display:flex;gap:8px;align-items:center}
 button{background:#2a2a3a;color:#ddd;border:1px solid #444;border-radius:6px;padding:6px 14px;cursor:pointer}
 button:hover{background:#3a3a55}
 input[type=range]{width:420px}
 .leg{font-size:12px;color:#889}
</style></head><body>
<h2 id="title"></h2><div id="hud"></div>
<div id="wrap"></div>
<div id="ctl">
 <button onclick="step(-1)">&#9664;&#9664;</button>
 <button id="pp" onclick="toggle()">&#9654;</button>
 <button onclick="step(1)">&#9654;&#9654;</button>
 <input type="range" id="scr" min="0" value="0" oninput="jump(this.value)">
 <span class="leg">蓝=我方 红=敌方 灰=中立 · 圈大小=发展度 · 三角=军团(蓝=我军 绿=盟军 红=敌军,大小=兵力,敌军仅视野内) · 箭头=本次命令 · 金环=战争目标(征服CB) · 黄圈=围城 · ⚔=首都 · 虚线=海峡</span>
</div>
<script>
const EPISODES = __EPISODES__;
let ep = 0, t = 0, playing = false, timer = null;

function epName(i){ return `第 ${i+1} 局 / ${EPISODES.length}`; }

function render(){
  const frames = EPISODES[ep];
  const f = frames[Math.min(t, frames.length-1)];
  const [w,h] = [960,520];
  const xs = EPISODES[ep].flatMap(fr=>fr.cells.map(c=>c[0]*5000));
  const ys = EPISODES[ep].flatMap(fr=>fr.cells.map(c=>c[1]*5000));
  const [minx,maxx,miny,maxy] = [Math.min(...xs),Math.max(...xs),Math.min(...ys),Math.max(...ys)];
  const pad=70, sx=(w-2*pad)/Math.max(maxx-minx,1), sy=(h-2*pad)/Math.max(maxy-miny,1), s=Math.min(sx,sy);
  const px=x=>pad+(x-minx)*s, py=y=>pad+(y-miny)*s;
  let g = `<svg width="${w}" height="${h}">`;
  for(const [a,b] of f.adj){
    const A=f.cells[a],B=f.cells[b];
    const isStrait = f.straits.some(p=>(p[0]===a&&p[1]===b)||(p[0]===b&&p[1]===a));
    const kind = isStrait?'strait':'edge';
    g+=`<line class="${kind}" x1="${px(A[0]*5000)}" y1="${py(A[1]*5000)}" x2="${px(B[0]*5000)}" y2="${py(B[1]*5000)}"/>`;
  }
  f.cells.forEach((c,i)=>{
    const our=c[4]>0.5,their=c[5]>0.5;
    const fill = our?'#3b6ea5':their?'#a53b3b':'#555560';
    const r = 8+c[2]*35*0.5;
    const terr=['平原','森林','丘陵','山地','沼泽'][c.slice(22,27).indexOf(1)];
    g+=`<circle class="p" cx="${px(c[0]*5000)}" cy="${py(c[1]*5000)}" r="${r}" fill="${fill}" fill-opacity="${c[3]>0.01?0.95:0.75}">`;
    g+=`<title>省 ${i}: ${terr}, dev ${Math.round(c[2]*35)}, 堡垒 ${Math.round(c[3]*3)}, 围城 ${(c[19]*20).toFixed(0)}/20</title></circle>`;
    if(c[3]>0.01) g+=`<rect x="${px(c[0]*5000)-3}" y="${py(c[1]*5000)-r-7}" width="6" height="4" fill="#c8b168"/>`;
    if(c[20]>0.5) g+=`<circle cx="${px(c[0]*5000)}" cy="${py(c[1]*5000)}" r="${r+6}" fill="none" stroke="#ffd24a" stroke-width="2.5" stroke-dasharray="3 3"/>`;
    if(c[21]>0.5||c[21]<-0.5) g+=`<text x="${px(c[0]*5000)}" y="${py(c[1]*5000)-r-6}" font-size="12" text-anchor="middle" fill="${c[21]>0.5?'#7fd0ff':'#ff9f8f'}">⚔</text>`;
    if(c[19]>0.02) g+=`<circle cx="${px(c[0]*5000)}" cy="${py(c[1]*5000)}" r="${r+3}" fill="none" stroke="#e8c14a" stroke-width="2" stroke-dasharray="${Math.max(1, c[19]*((r+3)*6.28))} 99"/>`;
    const player=(c[8]+c[9]+c[10])*14, ally=(c[12]+c[13]+c[14])*14, enemy=(c[16]+c[17]+c[18])*14;
    if(player>0.4) g+=`<path d="M0,-8L7,6L-7,6Z" transform="translate(${px(c[0]*5000)},${py(c[1]*5000)}) scale(${0.6+player/22})" fill="#7fd0ff" stroke="#0af"/>`;
    if(ally>0.4) g+=`<path d="M0,-8L7,6L-7,6Z" transform="translate(${px(c[0]*5000)-9},${py(c[1]*5000)-9}) scale(${0.6+ally/22})" fill="#7fe8a0" stroke="#3c3"/>`;
    if(enemy>0.4) g+=`<path d="M0,-8L7,6L-7,6Z" transform="translate(${px(c[0]*5000)+9},${py(c[1]*5000)+9}) scale(${0.6+enemy/22})" fill="#ff9f8f" stroke="#f43"/>`;
  });
  if(f.pick!==null && f.pick!==f.pos){
    const A=f.cells[f.pos],B=f.cells[f.pick];
    g+=`<defs><marker id="ar" markerWidth="8" markerHeight="8" refX="6" refY="3" orient="auto"><path d="M0,0L7,3L0,6z" fill="#ffe86a"/></marker></defs>`;
    g+=`<line x1="${px(A[0]*5000)}" y1="${py(A[1]*5000)}" x2="${px(B[0]*5000)}" y2="${py(B[1]*5000)}" stroke="#ffe86a" stroke-width="2.5" marker-end="url(#ar)"/>`;
  }
  if(f.pick!==null && f.pick===f.pos){
    const A=f.cells[f.pos];
    g+=`<circle cx="${px(A[0]*5000)}" cy="${py(A[1]*5000)}" r="16" fill="none" stroke="#ffe86a" stroke-width="2.5"/>`;
  }
  g+='</svg>';
  document.getElementById("wrap").innerHTML=g;
  document.getElementById('hud').innerHTML =
    `第 ${Math.floor(f.day)} 天 / 3600 &nbsp;|&nbsp; 战分 <b style="color:#8ce">${f.reward>=0?'+':''}${f.reward.toFixed(1)}</b> &nbsp;|&nbsp; 决策 ${t+1}/${frames.length}` +
    (f.done?` &nbsp;|&nbsp; <b style="color:#fd8">终局: ${f.done}</b>`:'');
  document.getElementById('title').textContent = `${epName(ep)} — ${f.tag}`;
  document.getElementById('scr').max = frames.length-1;
  document.getElementById('scr').value = Math.min(t,frames.length-1);
}
function step(d){ const n=EPISODES[ep].length; t=Math.max(0,Math.min(n-1,t+d)); render(); }
function jump(v){ t=+v; render(); }
function toggle(){ playing=!playing; document.getElementById('pp').innerHTML=playing?'&#10074;&#10074;':'&#9654;';
  clearInterval(timer); if(playing) timer=setInterval(()=>{ if(t>=EPISODES[ep].length-1){ if(ep<EPISODES.length-1){ep++;t=0;} else toggle(); } else t++; render(); }, 900); }
document.addEventListener('keydown',e=>{ if(e.key==='ArrowRight')step(1); if(e.key==='ArrowLeft')step(-1); if(e.key===' '){e.preventDefault();toggle();} if(e.key==='n'&&ep<EPISODES.length-1){ep++;t=0;render();} });
render();
</script></body></html>"""


def record(weights, exe, maps, episodes, out, tag):
	"""Play `episodes` playthroughs and dump the replay HTML."""
	policy = None
	if weights != "uniform":
		policy = TerritoryGNN()
		policy.load_state_dict(torch.load(weights, weights_only=True))
		policy.eval()
	all_frames = []
	for episode in range(episodes):
		p = sp.Popen(
			[exe, "--scenarios", str(episode + 1), "--rollouts", "1", "--maps", maps],
			stdin=sp.PIPE, stdout=sp.PIPE, stderr=sp.DEVNULL, text=True, encoding="utf-8")
		frames, done_stats = [], None
		for line in p.stdout:
			r = json.loads(line)
			if r.get("done"):
				done_stats = (
					f"reward {r['reward']:+.1f}, 占领 {r['taken']}, "
					f"战斗 {r['won']}W/{r['lost_battles']}L, 歼灭 {r['wipes']}"
				)
				continue
			nodes, edge_index, rules, cands = parse(r)
			cells = r["board"]["cells"]
			# Column 8 is our own infantry: nonzero only where the player
			# army stands, so its argmax is our position.
			pos = max(range(len(cells)), key=lambda i: cells[i][8])
			if policy is None:
				pick = (episode * 7 + len(frames) * 3) % len(cands)
			else:
				with torch.no_grad():
					pick = int(policy(nodes, edge_index, rules, cands).argmax())
			pick = min(pick, len(cands) - 1)
			frames.append({
				"cells": cells, "adj": r["board"]["adj"],
				"straits": [],
				"day": float(rules[3]) * 3600,
				"reward": r["reward"],
				"pos": pos, "pick": cands[pick],
				"candidates": cands, "done": None, "tag": tag,
			})
			p.stdin.write(json.dumps({"pick": pick}) + "\n")
			p.stdin.flush()
		p.wait()
		if frames:
			frames[-1]["done"] = done_stats or ""
			all_frames.append(frames)
	html = HTML.replace("__EPISODES__", json.dumps(all_frames))
	Path(out).write_text(html, encoding="utf-8")
	print(f"{out}: {len(all_frames)} episodes, "
		f"{sum(len(f) for f in all_frames)} decisions")


def main():
	import argparse
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--env", required=True)
	parser.add_argument("--weights", default="uniform")
	parser.add_argument("--maps", default="real")
	parser.add_argument("--episodes", type=int, default=4)
	parser.add_argument("--out", default="target/replay.html")
	parser.add_argument("--tag", default="policy")
	options = parser.parse_args()
	record(options.weights, options.env, options.maps,
		options.episodes, options.out, options.tag)


if __name__ == "__main__":
	main()
