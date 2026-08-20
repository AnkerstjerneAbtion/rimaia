import { useState } from "react";
import "./App.css";

function App() {
  const [count, setCount] = useState(0);

  return (
    <main className="container">
      <h1>Rimaia</h1>

      <div className="counter">
        <button aria-label="Decrease" onClick={() => setCount(count - 1)}>
          −
        </button>
        <span className="counter-value">{count}</span>
        <button aria-label="Increase" onClick={() => setCount(count + 1)}>
          +
        </button>
      </div>

      <button className="reset" onClick={() => setCount(0)}>
        Reset
      </button>
    </main>
  );
}

export default App;
