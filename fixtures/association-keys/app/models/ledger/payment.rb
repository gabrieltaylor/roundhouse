module Ledger
  class Payment < ApplicationRecord
    self.table_name = "receipts"

    has_many :entries, -> { where(active: true).order(position: :asc, id: :asc) }, class_name: "Entry", as: :payable, foreign_type: :payer_kind, foreign_key: "payable_slug", primary_key: "slug"
    has_many :invoices, -> { order(position: :asc) }, through: "entries", source: "invoice"
    has_many :bills, -> { order(position: :asc) }, through: :entries, source: :invoice, class_name: "ArchivedInvoice"
    has_many :legacy_bills, through: :entries, source: :invoice, class_name: "LegacyInvoice"
    has_many :audits, through: :entries, source: :audit_invoice
    has_many :documents, through: :entries, source: :documentable, source_type: "Ledger::Invoice"
    has_many :archived_documents, through: :entries, source: :documentable, source_type: "Ledger::Invoice", class_name: "ArchivedInvoice"
    has_one :first_entry, -> { order(position: :asc) }, class_name: "Entry", as: :payable, foreign_type: :payer_kind, foreign_key: :payable_slug, primary_key: :slug

    def invoice_codes
      invoices.map { |invoice| invoice.code }
    end

    def self.batch
      all.includes(:entries, :invoices, :bills, :audits, :documents, :archived_documents, :first_entry)
    end
  end
end
