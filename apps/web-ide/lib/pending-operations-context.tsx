'use client';

import { createContext, useContext, useCallback, useRef, useState, useEffect } from 'react';
import { useRouter } from 'next/navigation';

export interface PendingOperation {
  id: string;
  description: string;
  controller: AbortController;
  createdAt: number;
}

interface PendingOperationsContextType {
  addOperation: (description: string) => { id: string; controller: AbortController };
  removeOperation: (id: string) => void;
  hasPendingOperations: () => boolean;
  getPendingCount: () => number;
  abortAllOperations: () => void;
}

const PendingOperationsContext = createContext<PendingOperationsContextType | undefined>(undefined);

export function PendingOperationsProvider({ children }: { children: React.ReactNode }) {
  const operationsRef = useRef<Map<string, PendingOperation>>(new Map());
  const [_operationCount, setOperationCount] = useState(0);
  const router = useRouter();
  const pendingNavigationRef = useRef(false);

  const updateCount = useCallback(() => {
    setOperationCount(operationsRef.current.size);
  }, []);

  const addOperation = useCallback(
    (description: string): { id: string; controller: AbortController } => {
      const id = Math.random().toString(36).substring(2, 11);
      const controller = new AbortController();
      const operation: PendingOperation = {
        id,
        description,
        controller,
        createdAt: Date.now(),
      };
      operationsRef.current.set(id, operation);
      updateCount();
      console.log('Operation started:', description);
      return { id, controller };
    },
    [updateCount]
  );

  const removeOperation = useCallback(
    (id: string) => {
      const op = operationsRef.current.get(id);
      if (op) {
        console.log('Operation completed:', op.description);
        operationsRef.current.delete(id);
        updateCount();
      }
    },
    [updateCount]
  );

  const hasPendingOperations = useCallback(() => {
    return operationsRef.current.size > 0;
  }, []);

  const getPendingCount = useCallback(() => {
    return operationsRef.current.size;
  }, []);

  const abortAllOperations = useCallback(() => {
    console.log('Aborting', operationsRef.current.size, 'operations');
    operationsRef.current.forEach((op) => {
      op.controller.abort();
    });
    operationsRef.current.clear();
    updateCount();
  }, [updateCount]);

  useEffect(() => {
    const originalPush = router.push;
    const handleNavigation = async (path: string, options?: Record<string, unknown>) => {
      if (hasPendingOperations() && !pendingNavigationRef.current) {
        pendingNavigationRef.current = true;
        const operations = Array.from(operationsRef.current.values());
        const descriptions = operations.map((op) => `• ${op.description}`).join('\n');
        const count = operations.length;
        const msg = `Sono in corso ${count} operazione${count > 1 ? 'i' : ''}:\n\n${descriptions}\n\nVuoi annullarle e continuare?`;
        
        const confirmed = window.confirm(msg);
        if (!confirmed) {
          pendingNavigationRef.current = false;
          return;
        }
        abortAllOperations();
        pendingNavigationRef.current = false;
      }
      return (originalPush as unknown as (path: string, options?: Record<string, unknown>) => void).call(router, path, options);
    };

    (router as unknown as Record<string, unknown>).push = handleNavigation;
    return () => {
      (router as unknown as Record<string, unknown>).push = originalPush;
    };
  }, [router, hasPendingOperations, abortAllOperations]);

  useEffect(() => {
    const handleBeforeUnload = (e: BeforeUnloadEvent) => {
      if (hasPendingOperations()) {
        e.preventDefault();
        e.returnValue = 'Sono in corso operazioni. Se abbandoni la pagina verranno annullate.';
        return e.returnValue;
      }
    };
    window.addEventListener('beforeunload', handleBeforeUnload);
    return () => window.removeEventListener('beforeunload', handleBeforeUnload);
  }, [hasPendingOperations]);

  return (
    <PendingOperationsContext.Provider
      value={{
        addOperation,
        removeOperation,
        hasPendingOperations,
        getPendingCount,
        abortAllOperations,
      }}
    >
      {children}
    </PendingOperationsContext.Provider>
  );
}

export function usePendingOperations() {
  const context = useContext(PendingOperationsContext);
  if (!context) {
    throw new Error('usePendingOperations must be used within PendingOperationsProvider');
  }
  return context;
}
