"use client";

import { useCallback, useEffect, useState } from "react";
import {
  createProfile,
  deleteProfile,
  getProfiles,
  setDefaultProfile,
  updateProfile,
  type CreateProfilePayload,
  type UpdateProfilePayload,
  type UserProfile,
} from "./api-client";

export const DEFAULT_PROFILE_ID = "default";

export function useProfiles() {
  const [profiles, setProfiles] = useState<UserProfile[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const data = await getProfiles();
      setProfiles(data.profiles ?? []);
    } catch (e) {
      // Il fallimento era silenzioso: l'utente vedeva la lista vuota e
      // concludeva di non avere profili, invece che di non poterli leggere.
      // Lista vuota e API irraggiungibile sono due cose diverse e vanno dette.
      setError(e instanceof Error ? e.message : "Impossibile caricare i profili");
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const create = useCallback(async (payload: CreateProfilePayload) => {
    const profile = await createProfile(payload);
    setProfiles((prev) => [...prev, profile]);
    return profile;
  }, []);

  const update = useCallback(async (id: string, payload: UpdateProfilePayload) => {
    const profile = await updateProfile(id, payload);
    setProfiles((prev) => prev.map((p) => (p.id === id ? profile : p)));
    return profile;
  }, []);

  const remove = useCallback(async (id: string) => {
    await deleteProfile(id);
    setProfiles((prev) => prev.filter((p) => p.id !== id));
  }, []);

  const setDefault = useCallback(async (id: string) => {
    await setDefaultProfile(id);
    setProfiles((prev) => prev.map((p) => ({ ...p, isDefault: p.id === id })));
  }, []);

  const defaultProfile = profiles.find((p) => p.isDefault) ?? null;

  return { profiles, isLoading, error, defaultProfile, reload: load, create, update, remove, setDefault };
}
