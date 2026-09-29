module Ledger
  class Entry < ApplicationRecord
    self.table_name = :allocations

    belongs_to :invoice, class_name: "Invoice", foreign_key: "document_code", primary_key: "code"
    belongs_to :restricted_invoice, -> { where(position: 2) }, class_name: "Invoice", foreign_key: :document_code, primary_key: :code
    belongs_to :audit_invoice, class_name: "Invoice", foreign_key: :audit_code, primary_key: :code
    belongs_to :payable, polymorphic: true, foreign_key: :payable_slug, foreign_type: :payer_kind, primary_key: :slug
    belongs_to :documentable, polymorphic: true, foreign_key: :document_code, foreign_type: :document_type
  end
end
