"use client";
import { useState } from "react";
import {
  flexRender,
  getCoreRowModel,
  getSortedRowModel,
  useReactTable,
  type ColumnDef,
  type SortingState,
  type VisibilityState,
} from "@tanstack/react-table";
import { ArrowDownUp, Columns3 } from "lucide-react";
import { Empty } from "./ui";
export function DataTable<T>({
  rows,
  columns,
  visibility = false,
  label = "Observations",
}: {
  rows: T[];
  columns: ColumnDef<T>[];
  visibility?: boolean;
  label?: string;
}) {
  const [sorting, setSorting] = useState<SortingState>([]),
    [columnVisibility, setVisibility] = useState<VisibilityState>({});
  // React Compiler is not enabled. Keep TanStack's mutable table API un-memoized.
  // eslint-disable-next-line react-hooks/incompatible-library
  const table = useReactTable({
    data: rows,
    columns,
    state: { sorting, columnVisibility },
    onSortingChange: setSorting,
    onColumnVisibilityChange: setVisibility,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
  });
  return (
    <>
      {visibility && (
        <div className="table-tools">
          <span>Sorting applies to loaded rows only</span>
          <details>
            <summary>
              <Columns3 size={14} />
              Columns
            </summary>
            <div className="column-menu">
              {table
                .getAllLeafColumns()
                .filter((c) => c.getCanHide())
                .map((c) => (
                  <label key={c.id}>
                    <input
                      type="checkbox"
                      checked={c.getIsVisible()}
                      onChange={c.getToggleVisibilityHandler()}
                    />
                    {typeof c.columnDef.header === "string"
                      ? c.columnDef.header
                      : c.id}
                  </label>
                ))}
            </div>
          </details>
        </div>
      )}
      {!rows.length ? (
        <Empty title="No matching records">
          Try different filters, return to the previous page, or refresh after
          telemetry has persisted.
        </Empty>
      ) : (
        <div className="table-scroll">
          <table aria-label={label}>
            <thead>
              {table.getHeaderGroups().map((g) => (
                <tr key={g.id}>
                  {g.headers.map((h) => (
                    <th key={h.id}>
                      {h.column.getCanSort() ? (
                        <button
                          className="sort"
                          onClick={h.column.getToggleSortingHandler()}
                        >
                          {flexRender(
                            h.column.columnDef.header,
                            h.getContext(),
                          )}
                          <ArrowDownUp size={11} />
                          {h.column.getIsSorted() === "asc"
                            ? " ↑"
                            : h.column.getIsSorted() === "desc"
                              ? " ↓"
                              : ""}
                        </button>
                      ) : (
                        flexRender(h.column.columnDef.header, h.getContext())
                      )}
                    </th>
                  ))}
                </tr>
              ))}
            </thead>
            <tbody>
              {table.getRowModel().rows.map((r) => (
                <tr key={r.id}>
                  {r.getVisibleCells().map((c) => (
                    <td key={c.id}>
                      {flexRender(c.column.columnDef.cell, c.getContext())}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
