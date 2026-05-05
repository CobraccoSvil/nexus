"use client";

import { useParams } from "next/navigation";
import { SettingsPanel } from "../../../../components/settings-panel";

export default function CategorySettingsPage() {
  const params = useParams();
  const category = params.category as string;

  return <SettingsPanel category={category} />;
}
