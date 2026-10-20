import React from "react";

type Props = { title: string; count?: number };

export function Card({ title, count = 0 }: Props) {
  const label = count > 1 ? "items" : "item";
  return (
    <div className="card" onClick={() => console.log(title)}>
      <h1>{title}</h1>
      <Badge value={count} />
      {label}
    </div>
  );
}
