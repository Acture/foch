import "./App.css";
import type { JSX } from "react";

export default function App(): JSX.Element {
	return (
		<main className="shell">
			<section className="hero" aria-labelledby="app-title">
				<p className="eyebrow">EU4 mod workspace</p>
				<h1 id="app-title">Foch</h1>
				<p className="summary">
					Inspect an ordered playset, review every merge result, and publish only what
					you approve.
				</p>
			</section>

			<section className="status-card" aria-labelledby="status-title">
				<div>
					<p className="status-label">Desktop foundation</p>
					<h2 id="status-title">Application shell ready</h2>
				</div>
				<span className="status-badge">APP-001</span>
				<p className="status-copy">
					Steam, EU4, base-data, and current-playset detection arrive in the next
					product slice.
				</p>
			</section>
		</main>
	);
}
