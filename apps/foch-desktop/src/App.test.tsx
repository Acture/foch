import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import App from "./App";

describe("Foch desktop shell", (): void => {
	it("labels the current slice without claiming environment readiness", (): void => {
		render(<App />);

		expect(screen.getByRole("heading", { level: 1, name: "Foch" })).toBeInTheDocument();
		expect(
			screen.getByRole("heading", {
				level: 2,
				name: "Application shell ready",
			}),
		).toBeInTheDocument();
		expect(screen.getByText("APP-001")).toBeInTheDocument();
		expect(
			screen.getByText(/detection arrive in the next product slice/i),
		).toBeInTheDocument();
		expect(screen.queryByText(/^ready$/i)).not.toBeInTheDocument();
	});
});
